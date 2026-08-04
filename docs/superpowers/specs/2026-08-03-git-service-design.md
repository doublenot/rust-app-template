# Git service for `chrome-host-app` — design

Status: approved, ready to plan. Every git2 signature below is checked against **git2 0.21.0 / libgit2 1.9.6**, and the ones this document originally flagged were then re-checked directly against the vendored crate source at `~/.cargo/registry/src/*/git2-0.21.0/` (`merge_commits`, `merge_trees`, `treebuilder`, `Reference::shorthand`, `Branch::name`, `Cert::as_hostkey`, `CertHostkey::hash_sha256`, `CertificateCheckStatus::CertificatePassthrough`, both `git2::opts` timeout setters, `IndexConflict`/`IndexEntry`). One **[VERIFY]** remains, in §12.1: its dependency-resolution half is now closed (macOS does resolve `openssl-sys → openssl-src`), but only a real macOS build can confirm that the vendored OpenSSL compiles and links there.

---

## 0. Provenance

Decided by the user across five rounds of design questions on 2026-08-02/03. These are constraints, not proposals; a reviewer who disagrees with one is disagreeing with a decision, not with this document.

| # | decision |
|---|---|
| 1 | The `[server]` child drives git through the **host's loopback HTTP API**, authenticated with the existing `x-host-token`. |
| 2 | Gated by a **`[git]` section in `app.toml`, absent by default** — absent means `/api/git/*` 404s, no files are created, and no libgit2 code runs. |
| 3 | The host syncs **nothing of its own**. It is a generic executor; the caller decides what is in the tree. |
| 4 | **One writable registry file**, `<data-dir>/repos.json`, hand-editable by the template author and writable through the API. |
| 5 | Working trees at **`<data-dir>/repos/<id>/`**; the caller supplies only an id and the API returns the absolute path. |
| 6 | Credentials: **HTTPS token/PAT and SSH key**, supplied **per call** or **stored in the definition**; stored ones are `0600` and redacted on read. |
| 7 | Mutating operations are **async jobs** — `202` + job id, polled at `/api/git/jobs/:id`. |
| 8 | **One job per repo**; a second operation gets `409` carrying the running job's id. Different repos run in parallel. |
| 9 | Operations: define, init, clone, pull, push, **combined sync**, **status/inspect**, **explicit stage+commit**, **branch management**, **hard reset**, delete (`?purge=true` removes the tree), **force push behind an explicit flag**. |
| 10 | Merge policy: two-way last-writer-wins, **prefer local whole-file** on conflict, never conflict markers. |
| 11 | Commit identity: **caller-supplied, host defaults**. |
| 12 | Auto triggers: **on-demand by default**; `auto_sync_secs`, `sync_on_start`, `sync_on_quit` are per-repo opt-ins. |
| 13 | User surface: tray "Sync now", error dialogs, git state in `/api/status` — **all available, all default off**. |
| 14 | `sync_settings = true` per repo is the one built-in: the host mirrors `settings.json` in, validates the pulled copy against the schema, and restarts children. |
| 15 | `restart_children` is a **per-call flag, default false**, reusing the existing restart machinery. |
| 16 | Build: **fully vendored** libgit2 + libssh2 + TLS, so installers stay self-contained. |

Four follow-up calls made against this document after it was drafted:

- **`main.rs` is split first** (slice 0), as a pure no-behaviour-change move.
- **SSH is kept**, accepting that `perl` + `make` join the build prerequisites on Linux *and* macOS (§12.2) — this revises the "only a C toolchain" premise of decision 16, knowingly.
- **All twelve slices are planned as one body of work.**
- **A sync that resolved conflicts raises a dialog when `error_dialogs` is on** (§9.3) — overriding this document's original recommendation that a conflict-resolved sync stay silent.

---

## 1. Summary

The host process gains a git service backed by `git2` (vendored libgit2 + libssh2 + OpenSSL), exposed over the existing loopback axum server at `/api/git/*` behind the existing `x-host-token`, so the optional `[server]` child — in any language — can drive git without shipping a git client. The host is a **generic executor**: it syncs nothing of its own, working trees live at `<data-dir>/repos/<id>/`, and the API hands back absolute paths so the child reads and writes files directly with ordinary filesystem calls. The feature is gated by a `[git]` section in `app.toml` that is **absent by default** — with it absent, `/api/git/*` 404s, no directories or files are created, and no libgit2 code runs. Mutating operations return `202` plus a job id and execute on `spawn_blocking`, one job per repo (a second gets `409` with the running job's id), with a two-way **prefer-local** merge that never leaves conflict markers and always preserves the losing remote version as the merge commit's second parent. Out of the box git is invisible: a rotating `git.log` and the API; the tray entry, error dialogs, and `/api/status` exposure are three independent booleans, all defaulting off.

---

## 2. Architecture

### 2.1 Precondition commit — split `main.rs`

`src/main.rs` is 731 LOC today and this feature adds ~110. **Land this as its own no-behaviour-change commit before any git code**, so the git PR's diff shows that `kill_children`, `restart`, and the generation counters are untouched.

- **`src/host.rs`** (~600 LOC) ← `struct App`, `struct Children`, and every `impl App` method (`log`, `chrome_log`, `target_url`, `initial_chrome_url`, generation getters, `refresh_tray`, both watchers, `start_children`, `kill_children`, `restart`, `quit`, `restart_chrome_only`, `open_settings`) plus the four new git helpers. `App`'s fields become `pub(crate)`.
- **`src/main.rs`** (309 LOC — **measured**, see below) keeps `UserEvent`, `dialog`, `notice`, `missing_chrome_dialog`, `main()`, `run()`, and the `event_loop.run` dispatch `match`.

> **Measured on the actual split** (commits `4c7d2f3` + `c9ee6a9`): `host.rs` is **446** LOC and `main.rs` **309**, not the ~600/~260 estimated here. 309 is the *floor* for a pure move, not slack: crate header + `UserEvent` (30) + `dialog` + `notice` + `missing_chrome_dialog` (44) + `main()` (86) + `run()` and the dispatch `match` (147). Getting under 280 would require moving `App`'s construction or the dispatch arms into `host.rs`, which is a restructure rather than a move and would invalidate task 10's `run()`-signature change. The `host.rs` estimate was high because it counted the four git helpers, which land in later slices.

The cut is already latent: everything above `fn main()` is the host state machine, everything below is bootstrap + dispatch.

### 2.2 New tree: `src/git/`

| file | single responsibility | LOC (impl/test) | may name |
|---|---|---|---|
| `mod.rs` | `GitService` — the only type the rest of the host sees. Owns registry, state file, job store, paths, `git.log`, host instance id. `start()`, `sync_all_manual()`, `run_quit_syncs()`, `status_summary()`, id validation, path containment, `bytes_to_path`. | 190 / 70 | registry, state, jobs, ops, config, `HostEvent`, `supervisor::open_log`, tokio |
| `error.rs` | `GitError`, stable `code()` strings, `http_status()`, `retryable()`, `From<git2::Error>` classification, `scrub()`. No I/O. | 200 / 80 | git2 (types only), axum |
| `secret.rs` | `Secret(String)`: `Deserialize` **yes**, `Serialize` **deliberately absent**, `Debug` prints `Secret(***)`, value reachable only via `expose()`. | 60 / 30 | — |
| `registry.rs` | `repos.json` only: `RepoDef`, `CredentialSpec`, tolerant load, quarantine, atomic 0600 save, `RepoView` redaction. **No git2, no axum, no tokio, no jobs.** | 330 / 160 | serde, secret |
| `state.rs` | `<data-dir>/git-state.json`: durable `last_sync` per repo + learned SSH host fingerprints. Atomic 0600 save, best-effort (never fatal). | 130 / 60 | serde |
| `jobs.rs` | `JobStore`: ids, records, per-repo exclusion, `request_id` index, progress, retention. **No git2, no HTTP, no fs.** | 300 / 150 | std::sync, serde |
| `creds.rs` | `resolve()` (pure), `bound_host` enforcement, the `RemoteCallbacks` builder, attempt budget, SSH host-key TOFU. | 260 / 110 | registry, secret, git2 |
| `ops.rs` | Every blocking git2 verb: `init`, `clone`, `commit`, `pull`, `push`, `sync`, `status`, `branches`, `branch`, `reset`, `purge`. `fn(&OpCtx) -> Result<OpOutcome, GitError>`. No tokio, no axum. | 620 / 260 | git2, creds, merge, error |
| `merge.rs` | The prefer-local two-way merge and nothing else. | 240 / 130 | git2, error |
| `api.rs` | axum sub-router: DTOs, auth layer, error envelope. **No git2 type crosses this file.** | 520 / 230 | axum, `HostState`, all of the above |

New: **≈2 850 LOC (≈1 850 impl, ≈1 000 tests)**.

Boundary rationale — each file is reviewable alone:
- `jobs.rs` is a concurrency primitive over `FnOnce() -> Result<Value, GitError>`; you can audit the 409 path and eviction without knowing git exists, and its tests use closures blocking on a channel (deterministic, no sleeps).
- `registry.rs` is serde + `rename`; "can a malicious `id` escape `repos/`?" is 40 lines in one file.
- `creds.rs` splits into a pure `resolve()` returning a `CredChoice` *description* (table-tested, zero libgit2) and a ~20-line git2 adapter.
- `merge.rs` answers "can this ever write conflict markers?" in 240 lines.
- `api.rs` is a translation table — it is the file a Node author's questions map onto 1:1.

**`src/internal_server.rs` is not split** (366 → ~412). It gains a state field, one conditional `.nest()`, three `HostEvent` variants, and a `StatusResponse` wrapper. That is only true because the ~17 git handlers live in `git/api.rs`; inlined, the file would be ~800 LOC of two unrelated subsystems.

### 2.3 Changed files

| file | change | Δ |
|---|---|---|
| `Cargo.toml` | `git2`, target-gated `openssl-sys`, `gethostname` | +8 |
| `app.toml` | commented-out `[git]` block | +16 |
| `src/config.rs` | `GitSection`, `SshHostKeyPolicy`, `validate()` arm, `RuntimePaths::{repos_dir, registry_file, git_state_file}`, tests | +170 |
| `src/supervisor.rs` | `build_env` gains `host: &HostAccess` → `APP_HOST_URL`/`APP_HOST_TOKEN`/`APP_GIT_ENABLED` | +25 |
| `src/internal_server.rs` | `HostState.git: Option<Arc<GitService>>`, `.nest("/api/git", …)`, `authorized` → `pub(crate)`, `StatusResponse`, 3 `HostEvent` variants | +46 |
| `src/tray.rs` | `TrayAction::GitSync`, `menu_model(.., show_sync)` | +15 |
| `src/host.rs` | service construction, 3 event arms, tray arm, quit sync, `git_notice_is_new`, dialog gating | +125 |
| `src/main.rs` | `notice()` beside `dialog()` (warning-level, non-error modal) | +10 |
| `README.md` | §9 "Git service" (opens with the Node quickstart), `[git]` table in §3, §4 env contract, §5 security delta, §6 data-dir rows, §7 build prerequisites | +290 |
| `.github/workflows/*` | `perl` + `pkg-config` on the Linux apt line, build-time note | +6 |

### 2.4 Request flow

```
[server] child (node/python/…)
  │  POST $APP_HOST_URL/api/git/repos/notes/sync
  │  x-host-token: $APP_HOST_TOKEN   { "request_id": "boot-1" }
  ▼
axum (tokio worker)  ── git/api.rs
  │  route_layer: token check (401)  → id validation (422) → registry lookup (404)
  ▼
GitService::start_job()  ── git/mod.rs
  │  JobStore::admit(repo, op, request_id)  [ONE std::sync::Mutex]
  │    ├ Replay  → 200 {job}          (same request_id, live or succeeded)
  │    ├ Busy    → 409 {error.job}    (another job owns the repo)
  │    └ Started → RepoLease + JobId  ─────────────────┐
  │                                                     │  202 {job}  ← returns immediately
  ▼                                                     ▼
rt.spawn_blocking(move || {                     [caller polls GET /api/git/jobs/:id]
    let _lease = lease;          // RAII: releases the repo on return OR panic
    slot.begin();                // Running
    ops::run(op, &ctx)  ── git/ops.rs ── git/merge.rs ── git/creds.rs
        │  Repository::open / RepoBuilder::clone   ← libgit2 (blocking C)
        │  RemoteCallbacks{credentials, transfer_progress, certificate_check,
        │                  push_update_reference}
        │  progress → slot (throttled 100ms);  abort flag ← watchdog
        │  NO git2 value escapes this closure.  Repository held for the whole job.
    slot.succeed(v) | slot.fail(e);
    state.record_last_sync(); log_line(git.log);
    if restart_needed  → events.send(HostEvent::GitRestartChildren)  ──┐
    if ok→fail         → events.send(HostEvent::GitFailed)           ──┤
})                                                                     │
                                                                       ▼
                        existing bridge task in run():  host_rx → proxy.send_event
                                                                       ▼
                                        tao event loop (main thread) — App::restart()
```

**Dependency direction is strictly one-way**: `api → mod → {jobs, registry, state, ops} → {creds, merge, error, secret} → git2`. `ops`/`merge` never name axum, tokio, or `App`. `jobs` never names git2. Nothing in `src/git/` holds `App`, `Children`, `chrome_generation`, or `server_generation`.

---

## 3. Config — the `[git]` section

Absent by default. Shipped in `app.toml` as a comment block only. **Presence of the section — even empty — is the on switch**, exactly like `[server]`. There is no `enabled` key.

```toml
# Uncomment to give the host a git service. The [server] child reaches it at
# $APP_HOST_URL/api/git/* with header  x-host-token: $APP_HOST_TOKEN.
# Repos are defined in <data-dir>/repos.json; trees live in <data-dir>/repos/<id>/.
# See README §9.
# [git]
# tray_sync = false                 # add a "Sync now" tray entry
# error_dialogs = false             # native dialog when a sync starts failing,
#                                   # and when a sync overwrote a remote edit
# status_api = false                # add a "git" key to /api/status
# registry_writes = true            # false → repos.json is author-owned; PUT/DELETE 403
# default_branch = "main"
# author_name = ""                  # "" → [app].name
# author_email = ""                 # "" → "<identifier>@<hostname>"
# network_timeout_secs = 120
# quit_sync_timeout_secs = 10       # 0 disables sync_on_quit entirely
# allow_http = false                # permit plaintext http:// remotes
# ssh_host_key_policy = "tofu"      # "tofu" | "accept"
```

```rust
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitSection {
    #[serde(default)] pub tray_sync: bool,
    #[serde(default)] pub error_dialogs: bool,
    #[serde(default)] pub status_api: bool,
    #[serde(default = "yes")] pub registry_writes: bool,
    #[serde(default = "d_branch")] pub default_branch: String,
    #[serde(default)] pub author_name: String,
    #[serde(default)] pub author_email: String,
    #[serde(default = "d_net")]  pub network_timeout_secs: u64,
    #[serde(default = "d_quit")] pub quit_sync_timeout_secs: u64,
    #[serde(default)] pub allow_http: bool,
    #[serde(default)] pub ssh_host_key_policy: SshHostKeyPolicy,
}
fn yes() -> bool { true }
fn d_branch() -> String { "main".into() }
fn d_net() -> u64 { 120 }
fn d_quit() -> u64 { 10 }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SshHostKeyPolicy { #[default] Tofu, Accept }
```

`AppConfig` gains `#[serde(default)] pub git: Option<GitSection>` and `pub fn git_enabled(&self) -> bool { self.git.is_some() }`.

### Validation (appended to `AppConfig::validate`, plain `String` errors, existing voice)

| rule | error string |
|---|---|
| `default_branch` is a valid branch name (see below) | `git.default_branch "…" is not a valid branch name` |
| `author_name` has no `<`, `>`, `\n` | `git.author_name must not contain '<', '>' or newlines (git signatures cannot represent them)` |
| `author_email`, if non-empty, contains `@` and none of `<`, `>`, whitespace | `git.author_email "…" must look like a plain address, e.g. app@example.com` |
| `network_timeout_secs` ∈ `5..=3600` | `git.network_timeout_secs must be between 5 and 3600 (got 0)` |
| `quit_sync_timeout_secs` ≤ `120` | `git.quit_sync_timeout_secs must be 120 or less (0 disables sync_on_quit)` |

`validate_branch_name(s)` — shared with `repos.json` and `POST /branch`: non-empty, ≤ 200 bytes, every char in `[A-Za-z0-9._/-]`, no `..`, no leading `-` or `/`, no trailing `/` or `.lock`, no `//`, not `@`, no ASCII control.

Validation runs whether or not the section is later used, so enabling a repo never surfaces a new config error at an awkward moment.

### Deliberately constants, not keys

```rust
const MAX_JOB_RECORDS: usize      = 50;
const JOB_TTL_SECS: u64           = 3_600;
const JOB_MIN_AGE_SECS: u64       = 60;    // never evict a job younger than this
const AUTO_SYNC_MIN_SECS: u64     = 30;
const MAX_DIRTY_FILES: usize      = 200;
const CONNECT_TIMEOUT_MS: i32     = 10_000;
const MAX_CRED_ATTEMPTS: u32      = 2;
const PROGRESS_THROTTLE_MS: u64   = 100;
const RESTART_DEBOUNCE_MS: u64    = 2_000;
```

### Paths

`RuntimePaths` gains three fields, populated unconditionally (they are just paths):

```rust
pub repos_dir:       PathBuf,  // data_dir/repos/
pub registry_file:   PathBuf,  // data_dir/repos.json
pub git_state_file:  PathBuf,  // data_dir/git-state.json
```

`RuntimePaths::ensure()` is **unchanged**. `repos_dir` is created by `GitService::start()` and only then — decision 2's "no directories, no files, no libgit2 work when `[git]` is absent" is a property of where the `mkdir` lives.

---

## 4. Registry — `repos.json`

Location `<data-dir>/repos.json`, mode `0600` on unix. **Created only by the first successful `PUT`.** A host with `[git]` present and no repos never writes it.

**Invariant: this file holds definitions only.** It is never rewritten by a sync, a timer, or a job. Every host-written volatile value — last-sync outcome, learned SSH fingerprint — lives in `<data-dir>/git-state.json` (§4.5). That is what makes the file safe to hand-write, safe to commit to the template author's own repo, and keeps a credential-bearing file from being rewritten 2 880 times a day.

### 4.1 Example

```json
{
  "version": 1,
  "repos": [
    {
      "id": "notes",
      "remote": "https://github.com/acme/notes.git",
      "remote_name": "origin",
      "branch": "main",
      "credential": {
        "kind": "token",
        "username": "x-access-token",
        "token": "github_pat_11ABC…",
        "bound_host": "github.com"
      },
      "author": { "name": "Notes App", "email": "notes@acme.dev" },
      "sync_settings": false,
      "settings_path": "settings.json",
      "auto_sync_secs": 300,
      "sync_on_start": true,
      "sync_on_quit": false,
      "restart_children_on_pull": false,
      "created_at_ms": 1785000000000,
      "updated_at_ms": 1785000000000
    }
  ]
}
```

### 4.2 Field reference

| field | type | req | default | rules |
|---|---|---|---|---|
| `version` | int | yes | — | must be `1`; anything else ⇒ file-level failure (§4.4) |
| `repos[].id` | string | yes | — | §4.3 |
| `remote` | string\|null | no | `null` | `https://`, `ssh://`, `git://`, `file://`, `git@host:path`, or an absolute local path. `http://` rejected unless `[git].allow_http`. **Userinfo containing `:` (a password) is rejected** — libgit2 quotes URLs into error strings. `null` ⇒ local-only repo: `sync` inits+commits and reports `outcome: "committed"`; `clone`/`pull`/`push` fail `remote_missing` |
| `remote_name` | string | no | `"origin"` | `[A-Za-z0-9._-]{1,64}` |
| `branch` | string | no | `[git].default_branch` | `validate_branch_name` |
| `credential` | object\|null | no | `null` | §4.6 |
| `author.name` / `author.email` | string\|null | no | `null` | same char rules as the `[git]` keys |
| `sync_settings` | bool | no | `false` | `true` requires `AppConfig::settings_enabled()`, else the entry is rejected with `settings_sync_unavailable` — a silently no-op data-sync feature is how people lose data |
| `settings_path` | string | no | `"settings.json"` | relative, forward slashes, no `..`, no leading `/` |
| `auto_sync_secs` | int\|null | no | `null` | `null`/`0` = off; otherwise `30..=86400` |
| `sync_on_start` | bool | no | `false` | |
| `sync_on_quit` | bool | no | `false` | |
| `restart_children_on_pull` | bool | no | `false` | per-repo default for the request-level `restart_children` |
| `created_at_ms` / `updated_at_ms` | int | no | now | host-maintained; a hand-written file may omit them |

`deny_unknown_fields` everywhere, matching `config.rs`.

### 4.3 `id` validation

`^[a-z0-9][a-z0-9._-]{0,63}$`, **and** none of: contains `..`; ends with `.` or a space; equals `.`; is a Windows reserved name case-insensitively (`con prn aux nul com1-9 lpt1-9`).

**Lowercase only.** macOS and Windows filesystems are case-insensitive, so `Notes` and `notes` would collide into one directory on half of all machines. Making collision impossible beats making it platform-dependent. (Alternative: allow uppercase and normalize — cheap to relax later, expensive to tighten.)

### 4.4 Corrupt / partial file at startup

Two tiers, because a hand-written file deserves better than "we deleted it":

1. **File-level failure** — not valid JSON, top level not an object, `repos` not an array, `version != 1`, or duplicate ids. The file is **renamed** to `repos.json.corrupt-<epoch_ms>` (never deleted, never overwritten in place), the host starts with an empty in-memory registry, and `registry_error` is populated:
   ```json
   { "code": "registry_corrupt",
     "message": "repos.json is not valid JSON (line 12 column 3); quarantined",
     "quarantined_to": "/…/repos.json.corrupt-1785000000000" }
   ```
   Endpoints keep working; the first `PUT` writes a fresh file.

2. **Entry-level failure** — the file parses but an entry has a bad id, an unknown key, an out-of-range `auto_sync_secs`, or `sync_settings` with no schema. The bad entries are **skipped, not quarantined**, and reported:
   ```json
   { "code": "registry_entries_rejected",
     "message": "2 of 5 repo definitions were rejected",
     "rejected": [ { "index": 2, "id": "../evil", "reason": "invalid_repo_id" },
                   { "index": 4, "id": "cfg", "reason": "settings_sync_unavailable" } ] }
   ```
   The good entries load and work. **The rejected raw entries are retained as `serde_json::Value` and written back verbatim on every subsequent rewrite**, so fixing a typo never costs the author the rest of their file.

3. A stray `repos.json.tmp` is deleted and logged (a crash between `sync_all` and `rename`; the real file is intact).
4. `#[cfg(unix)]` a mode with group/other bits is tightened to `0600` in place and logged.
5. A file that parses to exactly what we would write is not rewritten (no gratuitous `updated_at_ms` churn).

`registry_error` appears in `GET /api/git`, in `/api/status` when `status_api = true`, and in `git.log` at startup. **The host itself always starts normally** — an optional feature must not brick the app.

**Not hot-reloaded.** Read once at startup, written by `PUT`/`DELETE`. Documented.

### 4.5 `git-state.json` — host-owned volatile state

```json
{ "version": 1,
  "repos": {
    "notes": {
      "last_sync": { "at_ms": 1785000004411, "ok": true, "op": "sync",
                     "job_id": "job_7c1e93aa_00002b", "outcome": "merged",
                     "head": "b4a71dd…", "code": null, "message": null },
      "ssh_host_fingerprint": "SHA256:nThbg6kXUpJWGl7E1IGOCspRomTxdCARLviKw6E5SY8"
    }
  } }
```

Written by the host only, atomically, `0600`, after every terminal job and after a TOFU fingerprint is learned. A corrupt or missing file is **not fatal**: it is logged and reset to empty. Never contains a secret. This is what makes decision 6's "last sync outcome" survive both job eviction and a host restart without touching `repos.json`.

### 4.6 Credentials and redaction

`#[serde(tag = "kind", rename_all = "snake_case")]`, `deny_unknown_fields`:

```jsonc
{ "kind": "none" }
{ "kind": "token",   "username": "x-access-token", "token": "ghp_…",
  "bound_host": "github.com" }
{ "kind": "ssh_key", "username": "git",
  "private_key_path": "/home/u/.ssh/id_ed25519",     // XOR with private_key
  "private_key": "-----BEGIN OPENSSH PRIVATE KEY-----\n…",
  "public_key_path": "/home/u/.ssh/id_ed25519.pub",
  "passphrase": "…",
  "bound_host": "github.com" }
```

`token`, `private_key`, `passphrase` are `Secret`. `username` defaults to `"x-access-token"` for `token` and `"git"` for `ssh_key`. `bound_host` is the lowercased host of `remote` at the moment the credential was stored (§8.2); it is host-written on define and must be present in any hand-written credential (or the host fills it from `remote` on load).

**A `private_key_path` pointing at a missing file is accepted at load** and surfaces as a per-job `auth_failed`. An unplugged USB key must not brick the whole registry.

Redaction is defended three ways, two of them structural:

1. **`Secret` has no `Serialize` impl.** Deriving `Serialize` on `RepoDef` is a compile error; the hand-written `CredentialView` is the only path to JSON.
2. **`RepoView`/`CredentialView` have no secret-shaped fields at all** — only `token_stored: bool`, `private_key_stored: bool`, `passphrase_stored: bool`, plus `has_credentials: bool`. You cannot leak by forgetting a `#[serde(skip)]` because there is nothing to skip. Paths are shown verbatim (they are not secrets, and hiding them makes misconfiguration undebuggable).
3. **`Secret`'s `Debug` prints `Secret(***)` and it has no `Display`.** A stray `dbg!(&def)` or a panic message cannot leak.

### 4.7 Atomic write

```rust
fn save(path: &Path, file: &RegistryFile) -> Result<(), GitError> {
    let tmp = path.with_extension("json.tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut f = open_private(&tmp)?;   // create_new(true), #[cfg(unix)] .mode(0o600)
    f.write_all(serde_json::to_string_pretty(file)?.as_bytes())?;
    f.sync_all()?;                     // durable BEFORE the rename
    drop(f);
    std::fs::rename(&tmp, path)?;      // atomic on POSIX; MOVEFILE_REPLACE_EXISTING on Windows
    #[cfg(unix)]
    { let _ = std::fs::File::open(path.parent().unwrap()).and_then(|d| d.sync_all()); }
    Ok(())
}
```

The temp file is created with mode `0600` **before** any bytes are written, so the token is never briefly world-readable. On Windows the file inherits the `%APPDATA%` ACL; documented in README §6 as a weaker posture, not implied away.

---

## 5. HTTP API

Base `/api/git`, nested onto the existing router **only when `[git]` is present**. When absent, a `/api/git/{*rest}` fallback is registered instead so the 404 is explicit:

```json
{ "error": { "code": "git_disabled", "message": "the host was built without a [git] section in app.toml" } }
```

**Every** route, including GETs, requires `x-host-token`, enforced by one `axum::middleware::from_fn_with_state` layer on the nested router — not per handler, so no route can forget it. This diverges from the unauthenticated `GET /api/status`; justified in §11 and stated in the README.

Timestamps are epoch milliseconds with an `_ms` suffix. (Alternative: ISO-8601 via the `time` crate — one dependency, one serializer; rejected to keep the template dependency-light.)

### 5.1 Endpoint table

| # | method | path | body | success | sync errors |
|---|---|---|---|---|---|
| 1 | GET | `/api/git` | — | 200 `ServiceInfo` | 401 |
| 2 | GET | `/api/git/repos` | — | 200 `{repos:[RepoView]}` | 401 |
| 3 | PUT | `/api/git/repos/:id` | `RepoDefBody` | 201 created / 200 replaced `{repo, warnings}` | 401, 403 `registry_read_only`, 409 `repo_busy`, 422 `invalid_repo_id`/`invalid_request`/`insecure_remote`/`settings_sync_unavailable`, 500 `registry_write_failed` |
| 4 | GET | `/api/git/repos/:id` | — | 200 `{repo, status}` | 401, 404, 422 |
| 5 | DELETE | `/api/git/repos/:id?purge=true` | — | 200 `{deleted:true, purged:bool, path}` | 401, 403 `registry_read_only`/`path_refused`, 404, 409 `repo_busy`, 500 `purge_failed` |
| 6 | GET | `/api/git/repos/:id/status` | — | 200 `RepoStatus` | 401, 404, 422, 504 `status_timeout` |
| 7 | GET | `/api/git/repos/:id/branches` | — | 200 `{current, detached, local:[], remote:[], upstream}` | 401, 404, 422 |
| 8 | POST | `/api/git/repos/:id/init` | `{request_id?}` | **202** `{job}` / 200 replay | 401, 404, 409, 422 |
| 9 | POST | `/api/git/repos/:id/clone` | `{request_id?, credential?}` | 202 / 200 | ″ + 422 `remote_missing` |
| 10 | POST | `/api/git/repos/:id/pull` | `{request_id?, credential?, commit_local?:false, restart_children?}` | 202 / 200 | ″ |
| 11 | POST | `/api/git/repos/:id/push` | `{request_id?, credential?, force?:false}` | 202 / 200 | ″ |
| 12 | POST | `/api/git/repos/:id/sync` | `{request_id?, credential?, message?, author?, allow_empty?:false, push?:true, force?:false, restart_children?}` | 202 / 200 | ″ |
| 13 | POST | `/api/git/repos/:id/commit` | `{request_id?, message?, author?, paths?:[], allow_empty?:false}` | 202 / 200 | ″ |
| 14 | POST | `/api/git/repos/:id/branch` | `{request_id?, name, create?:false, from?, checkout?:true, upstream?:string\|null}` | 202 / 200 | ″ |
| 15 | POST | `/api/git/repos/:id/reset` | `{request_id?, to?:"head"\|"upstream"\|<rev>, clean_untracked?:false, confirm:true}` | 202 / 200 | ″ + 422 `confirm_required` |
| 16 | GET | `/api/git/jobs?repo_id=&state=&limit=` | — | 200 `{jobs:[Job]}` newest first | 401, 422 |
| 17 | GET | `/api/git/jobs/:job_id` | — | 200 `{job}` | 401, 404 `job_not_found` |

Notes on shape:

- **Reads (4, 6, 7) are synchronous, not jobs.** They run in a `spawn_blocking` bounded at 2 s (`status_timeout`) and **deliberately bypass the per-repo lock** — observing a repo while it syncs is the main reason you'd call them. A read taken mid-checkout is a snapshot; the job's `result` is authoritative.
- **`PUT` is idempotent create-or-replace, whole-object.** No POST/PATCH split: one verb, no merge rules to document, and — usefully — no way to repoint an existing repo's `remote` while silently retaining its stored credential (§8.2 closes that anyway with `bound_host`). A `PUT` on a busy repo is 409: the job snapshotted its `RepoDef` at admission, so a replace mid-job would silently not apply.
- **14 collapses create-branch, checkout, and set-upstream** into one verb. `create:false, checkout:false, upstream` absent → 422 `invalid_request` ("nothing to do").
- **No cancel endpoint.** libgit2 is interruptible only inside the transfer callbacks; a cancel that works during fetch and silently no-ops during merge, checkout, and TCP connect is worse than none, because callers write retry logic against a promise the system cannot keep. It is replaced by `network_timeout_secs` (enforced in the callbacks we already need for progress), the process-global libgit2 connect/idle timeouts (§7.1), and a visible `stalled` flag. *(Alternative: `POST /jobs/:id/cancel` returning `{"requested":true}` — ~30 LOC, add if a real integrator asks.)*

### 5.2 `request_id` — the retry contract

Every mutating call may carry `request_id` (free-form, ≤128 printable-ASCII chars), scoped `(repo_id, request_id)`. Admission order inside one lock:

1. GC expired terminal jobs.
2. **Replay check, before the busy check.** If `(repo, rid)` maps to a job:
   - the job is **running** → `200 {job}` (the same job). This is the dropped-response case.
   - the job is **succeeded** → `200 {job}` (the same job). The effect already happened.
   - the job is **failed** → the index entry is **consumed**; fall through and admit a fresh job. *Retrying a failed operation with the same id must work* — this is the natural client behaviour and the mechanism must not lock it out.
3. Busy check → `409` with the running job embedded.
4. Insert, mark busy, index `request_id`, return `Started`.

Replay-before-busy is what makes "my fetch timed out, is it safe to retry?" unambiguous: a matching `request_id` can never come back as 409, so a 409 always means *someone else* holds the repo.

### 5.3 `host_instance` — the restart contract

Jobs are in-memory only. There is no job database, and the API says so rather than pretending otherwise.

- `host_instance` is 8 random hex chars minted once per host launch. It appears in `GET /api/git`, in every job body, and in the `job_not_found` envelope.
- `JobId` = `job_<host_instance>_<seq:06x>`. A lookup whose instance prefix differs from the current one is rejected immediately as `job_not_found`, so a surviving child can never be answered about a different job that happens to share a sequence number.
- **Recipe (documented in README §9):** on `job_not_found` where the envelope's `host_instance` differs from the one you saw → the host restarted; `GET /api/git/repos/:id` for `last_sync` and re-issue. Same `host_instance` → the job was evicted by retention; `last_sync` in the repo status still has the outcome.
- All operations are effect-idempotent enough for a blind re-run: a commit that already happened is a no-op (tree-id compare), a push that already landed reports `up_to_date`, `clone` on an existing tree reuses it.

### 5.4 Error envelope

Uniform on every non-2xx from `/api/git/*`:

```json
{ "error": { "code": "repo_busy",
             "message": "repo \"notes\" is busy running sync",
             "repo_id": "notes",
             "host_instance": "7c1e93aa",
             "job": { "id": "job_7c1e93aa_00002b", "op": "sync", "state": "running",
                      "stalled": false, "started_at_ms": 1785000000205,
                      "request_id": "boot-1" } } }
```

`error.job` is present only on `repo_busy`; `409` also carries `Retry-After: 1`. Existing endpoints (`/api/settings`, `/api/restart`) keep their bare-string bodies — deliberately not retrofitted (§13).

### 5.5 Representative bodies

**`PUT /api/git/repos/notes`**

```json
{ "remote": "https://github.com/acme/notes.git",
  "branch": "main",
  "remote_name": "origin",
  "credential": { "kind": "token", "username": "x-access-token", "token": "github_pat_xxx" },
  "author": { "name": "Notes App", "email": "notes@acme.dev" },
  "sync_settings": false,
  "auto_sync_secs": 300,
  "sync_on_start": true,
  "sync_on_quit": false }
```

→ **201**

```json
{ "repo": {
    "id": "notes",
    "path": "/home/u/.local/share/com.example.chromehost/repos/notes",
    "exists": false,
    "remote": "https://github.com/acme/notes.git",
    "remote_name": "origin", "branch": "main",
    "credential": { "kind": "token", "username": "x-access-token",
                    "token_stored": true, "bound_host": "github.com" },
    "has_credentials": true,
    "author": { "name": "Notes App", "email": "notes@acme.dev" },
    "sync_settings": false, "settings_path": "settings.json",
    "auto_sync_secs": 300, "sync_on_start": true, "sync_on_quit": false,
    "restart_children_on_pull": false,
    "created_at_ms": 1785000000000, "updated_at_ms": 1785000000000 },
  "warnings": [] }
```

`warnings` is a usually-empty array of `{code, message}`; it never changes the status code. Emitted for `auth_unbound` (a `PUT` changed the remote's host, so the stored credential was dropped) and `auto_sync_clamped`.

**`POST /api/git/repos/notes/sync`** `{"request_id":"boot-1"}` → **202**, `Location: /api/git/jobs/job_7c1e93aa_00002b`

```json
{ "job": { "id": "job_7c1e93aa_00002b", "host_instance": "7c1e93aa",
           "repo_id": "notes", "op": "sync", "state": "running", "phase": "preparing",
           "stalled": false, "progress": null, "request_id": "boot-1",
           "created_at_ms": 1785000000200, "started_at_ms": 1785000000205,
           "finished_at_ms": null, "result": null, "error": null } }
```

**`GET /api/git/jobs/job_7c1e93aa_00002b`** mid-flight

```json
{ "job": { "…": "…", "state": "running", "phase": "fetching", "stalled": false,
  "progress": { "received_objects": 812, "total_objects": 4211, "indexed_objects": 640,
                "received_bytes": 1048576, "percent": 15 },
  "result": null, "error": null } }
```

succeeded (a real prefer-local merge)

```json
{ "job": { "…": "…", "state": "succeeded", "phase": "finalizing",
  "finished_at_ms": 1785000004411, "progress": null,
  "result": {
    "outcome": "merged",
    "branch": "main", "head_before": "9c1f0ae…", "head_after": "b4a71dd…",
    "committed": true, "commit": "3ee0c21…", "files_committed": 3,
    "fetched": true, "fetched_head": "77a90b2…",
    "merge": { "kind": "merge_commit", "merge_commit": "b4a71dd…",
               "conflicts_resolved": ["notes/todo.md"],
               "recover_hint": "git show b4a71dd^2:notes/todo.md" },
    "pushed": true, "force_pushed": false, "upstream_set": false,
    "ahead": 0, "behind": 0,
    "settings_synced": false, "settings_rejected": null,
    "restart_requested": false,
    "warnings": [] },
  "error": null } }
```

`outcome` ∈ `up_to_date | no_changes | committed | fast_forward | merged | merged_unrelated | cloned | initialized | pushed | reset | checked_out | branch_created`.

failed — note the failure is **in the job**, not in an HTTP status; `GET /jobs/:id` returns 200

```json
{ "job": { "…": "…", "state": "failed", "finished_at_ms": 1785000002100, "result": null,
  "error": { "code": "not_fast_forward",
             "message": "remote rejected refs/heads/main: non-fast-forward; pull first or retry with force=true",
             "retryable": true,
             "git2": { "class": "Reference", "code": "NotFastForward" } } } }
```

**`GET /api/git/repos/notes/status`**

```json
{ "repo": { "…redacted definition…" },
  "status": {
    "id": "notes",
    "path": "/home/u/.local/share/com.example.chromehost/repos/notes",
    "exists": true, "is_repository": true,
    "state": "dirty",
    "branch": "main", "head": "b4a71dd…", "unborn": false, "detached": false,
    "upstream": "origin/main", "ahead": 1, "behind": 0,
    "dirty": { "modified": ["inbox.md"], "added": [], "deleted": [], "renamed": [],
               "untracked": ["scratch.txt"], "count": 2, "truncated": false },
    "remote": { "name": "origin", "url": "https://github.com/acme/notes.git" },
    "auto": { "auto_sync_secs": 300, "sync_on_start": true, "sync_on_quit": false },
    "busy_job": null,
    "last_sync": { "at_ms": 1785000004411, "ok": true, "op": "sync",
                   "job_id": "job_7c1e93aa_00002b", "outcome": "merged",
                   "head": "b4a71dd…", "code": null, "message": null } } }
```

`status.state` ∈ `absent | uninitialized | unborn | clean | dirty | detached | error`. On a broken tree, `status.error` is populated and the request still returns 200 — a broken tree must remain *inspectable*. `dirty` is capped at `MAX_DIRTY_FILES = 200` per bucket with `truncated: true`; a caller who drops `node_modules` into the tree must not be able to make the host serialize a 200 MB response.

**`DELETE /api/git/repos/notes?purge=true`** refusal

```json
HTTP/1.1 403 Forbidden
{ "error": { "code": "path_refused",
             "message": "refusing to purge \"/home/u/secret\": resolves outside …/repos/",
             "repo_id": "notes", "path": "/home/u/secret" } }
```

**`GET /api/git`**

```json
{ "host_instance": "7c1e93aa",
  "repos_root": "/home/u/.local/share/com.example.chromehost/repos",
  "registry_file": "/home/u/.local/share/com.example.chromehost/repos.json",
  "log_file": "/home/u/.local/share/com.example.chromehost/logs/git.log",
  "registry_writable": true, "registry_error": null,
  "defaults": { "branch": "main",
                "author": { "name": "Chrome Host App", "email": "com.example.chromehost@laptop" },
                "network_timeout_secs": 120 },
  "features": { "tray_sync": false, "error_dialogs": false, "status_api": false },
  "job_retention": 50, "job_ttl_secs": 3600, "job_min_age_secs": 60,
  "repo_count": 1 }
```

### 5.6 Error taxonomy

Two surfaces, **one vocabulary**: a synchronously-detectable problem becomes an HTTP status *and* a `code`; a problem discovered inside a job returns 202 and the same `code` string appears in `job.error.code`. A client writes one `match` and uses it in both places. `git2.class`/`git2.code` are carried through as debugging hints and are explicitly not part of the contract.

**Admission (synchronous HTTP)**

| code | HTTP | condition |
|---|---|---|
| `git_disabled` | 404 | `[git]` absent |
| `unauthorized` | 401 | missing/wrong `x-host-token` |
| `invalid_repo_id` | 422 | id fails §4.3 |
| `invalid_request` | 422 | body/DTO validation; `error.field` names it |
| `insecure_remote` | 422 | `http://` with `allow_http = false`, unsupported scheme, or a URL with a userinfo password |
| `settings_sync_unavailable` | 422 | `sync_settings: true` with no settings schema |
| `remote_missing` | 422 | `clone`/`pull`/`push` on a repo with `remote: null` |
| `confirm_required` | 422 | `reset` without `confirm: true` |
| `repo_not_found` | 404 | id not in the registry |
| `repo_busy` | 409 | a live job owns the repo |
| `job_not_found` | 404 | unknown, evicted, or foreign-instance job id |
| `registry_read_only` | 403 | `registry_writes = false` |
| `path_refused` | 403 | purge target resolves outside `<data-dir>/repos/` |
| `registry_corrupt` / `registry_entries_rejected` | — | reported in `registry_error`, never as a status |
| `registry_write_failed` | 500 | `repos.json` could not be persisted |
| `purge_failed` | 500 | tree removal failed |
| `status_timeout` | 504 | the bounded status read exceeded 2 s |
| `internal` | 500 | anything unmapped; message scrubbed |

**Execution (`job.error.code`, HTTP 200 on the poll)**

| code | retryable | source |
|---|---|---|
| `auth_failed` | false | `ErrorCode::Auth`; `ErrorClass::{Http,Ssh,Callback}` with an auth-shaped message |
| `auth_missing` | false | remote demanded auth, nothing usable configured (our callback) |
| `auth_unbound` | false | stored credential's `bound_host` ≠ the remote's host (§8.2) |
| `host_key_mismatch` | false | `ErrorCode::Certificate` + `ErrorClass::Ssh` (TOFU pin) |
| `certificate_invalid` | false | `ErrorCode::Certificate` + `ErrorClass::Ssl` |
| `network_failed` | **true** | classes `Net`, `Http`, `Ssl`, `Zlib`, `Indexer` |
| `timeout` | **true** | our watchdog aborted a transfer, or libgit2's own connect/idle timeout |
| `canceled` | — | abort flag observed (shutdown) |
| `not_fast_forward` | **true** | `ErrorCode::NotFastForward` (client-side) **or** `push_update_reference` reporting non-ff |
| `push_rejected` | **true** | `push_update_reference` with a server status (hook, protected branch) |
| `remote_not_found` | false | `ErrorCode::NotFound` with class `Net`/`Reference` |
| `no_worktree` | false | operation needs a tree; run `clone`/`init` first |
| `already_exists` | false | `ErrorCode::Exists` — init/clone into a non-empty directory |
| `not_a_repository` | false | dir exists, non-empty, no `.git` |
| `branch_mismatch` | false | HEAD is not on `def.branch` (synthesised) |
| `detached_head` | false | mutating op with a detached HEAD |
| `unborn_branch` | false | `ErrorCode::UnbornBranch` where a commit was required |
| `dirty_tree` | false | `ErrorCode::{Uncommitted,IndexDirty}`, or pull/checkout/reset would lose work |
| `merge_path_type_conflict` | false | a path is a file on one side and a directory on the other (§7.6) |
| `merge_unresolvable` | false | conflicts survived resolution; `ErrorCode::{Unmerged,Conflict,MergeConflict}` |
| `repo_locked` | **true** | `ErrorCode::Locked`; message names the lock file's absolute path |
| `settings_invalid` | false | (never fatal today — surfaces as `result.settings_rejected`) |
| `io_failed` | maybe | classes `Os`, `Filesystem`, `std::io::Error` |
| `internal` | false | fallback |

```rust
fn classify(e: &git2::Error) -> Code {
    use git2::{ErrorClass as K, ErrorCode as C};
    match (e.code(), e.class()) {
        (C::Auth, _)                             => Code::AuthFailed,
        (C::Certificate, K::Ssh)                 => Code::HostKeyMismatch,
        (C::Certificate, _)                      => Code::CertificateInvalid,
        (C::NotFastForward, _)                   => Code::NotFastForward,
        (C::Locked, _)                           => Code::RepoLocked,
        (C::Exists, _)                           => Code::AlreadyExists,
        (C::UnbornBranch, _)                     => Code::UnbornBranch,
        (C::Uncommitted, _) | (C::IndexDirty, _) => Code::DirtyTree,
        (C::Unmerged, _) | (C::Conflict, _) | (C::MergeConflict, _) => Code::MergeUnresolvable,
        // Verified: a transfer_progress callback returning `false` surfaces as
        // code=GenericError, class=Callback ("indexer progress callback returned -1"),
        // NOT as ErrorCode::User. Our own abort flag disambiguates timeout vs cancel.
        (_, K::Callback)                         => Code::from_abort_flag(),
        (C::NotFound, K::Net) | (C::NotFound, K::Reference) => Code::RemoteNotFound,
        (C::NotFound, K::Repository)             => Code::NoWorktree,
        (_, K::Net | K::Http | K::Ssl | K::Zlib | K::Indexer) => Code::NetworkFailed,
        (_, K::Os | K::Filesystem)               => Code::IoFailed,
        _                                        => Code::Internal,
    }
}
```

**`auth_failed` maps to HTTP 502 when it can be surfaced synchronously, never 401.** The request's own `x-host-token` authentication succeeded; the *upstream* rejected. Returning 401 would make a client's generic "refresh my token and retry" middleware do exactly the wrong thing. Called out in the README because it is the one surprising row.

---

## 6. Job engine

### 6.1 Types

```rust
pub struct JobId(String);   // "job_<host_instance:8hex>_<seq:06x>"

#[derive(Clone, Copy, Serialize)] #[serde(rename_all = "snake_case")]
pub enum JobOp { Init, Clone, Pull, Push, Sync, Commit, Branch, Reset }

#[derive(Clone, Copy, PartialEq, Serialize)] #[serde(rename_all = "snake_case")]
pub enum JobState { Running, Succeeded, Failed }

#[derive(Clone, Copy, Serialize)] #[serde(rename_all = "snake_case")]
pub enum Phase { Preparing, Cloning, Staging, Committing, Fetching, Merging,
                 CheckingOut, Pushing, Finalizing }

#[derive(Clone, Copy, Default, Serialize)]
pub struct Progress { pub received_objects: u32, pub total_objects: u32,
                      pub indexed_objects: u32, pub received_bytes: u64, pub percent: u8 }

pub struct JobSlot {                       // identity + the flags outside the mutex
    pub id: JobId, pub repo_id: String, pub op: JobOp,
    pub request_id: Option<String>, pub created_at_ms: u64,
    pub abort: Arc<AtomicBool>,            // read by the git2 progress callbacks
    pub stalled: Arc<AtomicBool>,          // set by the watchdog
    live: std::sync::Mutex<JobLive>,
}
struct JobLive { state: JobState, phase: Phase, progress: Option<Progress>,
                 started_at_ms: u64, finished_at_ms: Option<u64>,
                 result: Option<serde_json::Value>, error: Option<GitError>,
                 last_progress_write: Instant }

pub struct JobStore { inner: std::sync::Mutex<Inner>, instance: String, seq: AtomicU64 }
struct Inner {
    jobs:       BTreeMap<JobId, Arc<JobSlot>>,
    terminal:   VecDeque<JobId>,                    // oldest first
    busy:       BTreeMap<String, JobId>,            // repo_id -> live job  ← THE LOCK
    by_request: BTreeMap<(String, String), JobId>,  // (repo_id, request_id)
}
```

**Three states, not four.** There is no `Queued`: decision 10 makes a queued job unrepresentable — a job either exists and is running or was never created. `spawn_blocking` can briefly delay actual start under pool pressure; that is invisible and does not deserve a state.

**One flat `std::sync::Mutex`, no nesting, no actor.** `std::sync` and not `tokio::sync` because the only writer of `Progress` is a libgit2 callback on a blocking thread with no runtime context, and it must never `.await`. Every critical section is a handful of field writes; the nested `Mutex<Map<Id, Arc<Mutex<Record>>>>` shape would buy nothing and introduce a lock-ordering hazard between the progress callback and the completion path. `admit()` needs the lock only across check → allocate → mark-busy; the spawn happens after the guard is dropped, which is why a single-owner actor is unnecessary here.

**Per-repo mutual exclusion is a `BTreeMap` entry**, not a mutex per repo: no allocation, no lifetime question, no deadlock possible, no poisoning from a panicking job. Different repos are parallel for free because they are different keys.

### 6.2 Admission and the RAII lease

```rust
pub enum Admission {
    Started(Arc<JobSlot>, RepoLease),   // caller must run the op; lease released on drop
    Replay(Arc<JobSlot>),               // -> 200, not 202
    Busy(Arc<JobSlot>),                 // -> 409 with that job embedded
}
pub fn admit(&self, repo_id: &str, op: JobOp, request_id: Option<&str>) -> Admission;

pub struct RepoLease { store: Arc<JobStore>, repo_id: String, job: Arc<JobSlot> }
impl Drop for RepoLease {
    fn drop(&mut self) {
        // A non-terminal slot here means the closure panicked or unwound: no other
        // path reaches this state, so recording `internal` is unambiguous, not a guess.
        self.job.finish_if_running(GitError::internal("git worker terminated unexpectedly"));
        let mut inner = self.store.inner.lock().unwrap();
        inner.busy.remove(&self.repo_id);
        inner.terminal.push_back(self.job.id.clone());
        inner.evict();
    }
}
```

**The lease is released exactly when the blocking closure returns** — the instant the job becomes terminal and not one microsecond earlier. Caller-visible consequence: after you observe a terminal state, the next POST for that repo cannot 409.

### 6.3 Running a blocking git2 call

```rust
impl GitService {
    pub fn start_job(self: &Arc<Self>, repo_id: &str, op: JobOp, req: OpRequest)
        -> Result<Admission, GitError>
    {
        let def = self.registry.snapshot(repo_id)?;          // cheap clone; no lock held later
        let adm = self.jobs.admit(repo_id, op, req.request_id.as_deref());
        let Admission::Started(slot, lease) = adm else { return Ok(adm) };

        let cred = creds::resolve(req.credential.as_ref(), def.credential.as_ref(),
                                  def.remote.as_deref())?;
        let ctx = OpCtx {                       // fully owned; borrows nothing
            tree: self.paths.repos_dir.join(repo_id),
            def: def.clone(), cred, request: req.clone(),
            identity: self.identity.clone(),
            host_fingerprint: self.state.fingerprint(repo_id),
            abort: slot.abort.clone(), slot: slot.clone(),
            deadline: Instant::now() + Duration::from_secs(self.cfg.network_timeout_secs),
            settings: self.settings_ctx.clone(),
            #[cfg(test)] gate: self.test_gate.clone(),
        };

        // Watchdog: trips the abort flag so a wedged transfer ends as `timeout`
        // instead of pinning a blocking thread forever. The lease is NOT released.
        self.spawn_watchdog(slot.clone(), self.cfg.network_timeout_secs);

        let this = self.clone();
        // No rt.enter() guard: that is required only for tokio::process::Command::spawn,
        // which registers a child with the runtime's signal driver. Git spawns no
        // processes. Handle::spawn_blocking is callable from any thread.
        self.rt.spawn_blocking(move || {
            let _lease = lease;                 // dropped last -> releases the repo
            let outcome = ops::run(op, &ctx);   // the ONLY git2 code
            match &outcome {
                Ok(v)  => ctx.slot.succeed(v),
                Err(e) => ctx.slot.fail(e),
            }
            this.after_job(&def, op, &ctx, outcome);   // state file, git.log, HostEvents
        });
        Ok(Admission::Started(slot, RepoLease::taken()))
    }
}
```

**Hard rule, enforced by the module boundary and by review: no `git2` value ever escapes the closure.** `Repository` is opened inside `ops::run` and dropped there. Nothing `git2` is stored in the service, in an `Arc`, or held across an `.await`. That makes `Repository: !Sync` a non-issue and guarantees no leaked handles or lingering `index.lock`. Corollary from the reference: **keep the `Repository` alive for the whole job** — dropping it while an `Index` is still held detaches the index and `write_tree()` then fails.

`spawn_blocking` also contains panics: a libgit2 panic kills one job, the lease releases via `Drop`, and the host keeps running.

### 6.4 Progress

```rust
impl OpCtx {
    pub fn phase(&self, p: Phase);
    pub fn transfer(&self, p: &git2::Progress) -> bool;   // false => abort
}
```

`transfer` returns `false` immediately if `abort` is set; otherwise it writes to the slot only if `PROGRESS_THROTTLE_MS` has elapsed or `percent` changed — a 200 k-object fetch would otherwise thrash a mutex 200 k times. `percent = indexed_objects * 100 / total_objects` when `total_objects > 0`.

Push uses `push_transfer_progress(|cur, total, bytes| …)`, which returns `()` — the deadline there is enforced by the fetch phase preceding it and by libgit2's global idle timeout.

### 6.5 Retention

On every insert and every `GET /api/git/jobs*`:

1. Drop terminal jobs older than `JOB_TTL_SECS` (3 600).
2. While `terminal.len() > MAX_JOB_RECORDS` (50), pop the oldest.
3. **Never evict a live job**, regardless of count.
4. **Never evict a terminal job younger than `JOB_MIN_AGE_SECS` (60)**, regardless of count. This closes a real race: a client gets 202, and before it polls, a burst of auto-syncs evicts its record.
5. Evicting a job drops its `by_request` entry (the id becomes reusable).

**Published contract**, in `GET /api/git` and the README: *poll within `job_ttl_secs`; results younger than 60 s are always available.* Evicted ids 404 as `job_not_found` — which is why `last_sync` lives in `git-state.json`: the durable outcome survives eviction *and* a process restart.

### 6.6 Shutdown

`std::process::exit(0)` runs no destructors and does not join blocking threads. That is already true of this template and is acceptable here: libgit2 writes objects and refs via write-to-temp + rename, so an abandoned job leaves a **valid** repository, at worst missing the last operation. The one durable residue is `.git/index.lock`.

- `App::quit()` order becomes: `log("quit")` → **`kill_children()`** → `git.run_quit_syncs(deadline)` → `exit(0)`. Children die *first* so the tree is quiescent and the final commit is a consistent snapshot, and so the window closes immediately rather than after a network wait.
- `run_quit_syncs` sets `draining = true` (every later `admit` returns `shutting_down` → 503), enqueues one `sync` per `sync_on_quit` repo that is idle, runs them **concurrently**, and blocks on `rt.block_on(timeout(quit_sync_timeout_secs, join_all(...)))`. `0` disables the whole step. After the deadline it sets every `abort` flag, waits a further 2 s, logs `shutdown: N job(s) abandoned` with their ids, and exits.
- **Startup recovery, once, before any job may run**, for each registered repo whose tree exists: delete `<tree>/.git/index.lock` **only if its mtime predates process start**, and log it. Safe because the host holds `app.lock` (single instance) and is the declared owner of `<data-dir>/repos/`; the mtime guard covers the documented-but-possible case of a user running `git` by hand in the tree right now. **No automatic `reset --hard` and no automatic `cleanup_state()`** — our merge never enters `RepositoryState::Merge` (§7.6), so a merge state in one of these trees was created by a human, and silently discarding their staged resolution would be unrecoverable. A non-`Clean` repo state is logged and surfaced as `status.state = "error"`; the caller fixes it with `POST /reset`.

---

## 7. Git operations

### 7.1 Process-global setup (once, in `GitService::start`)

```rust
// Without these a black-holed remote pins a spawn_blocking thread indefinitely:
// our watchdog lives inside the transfer callbacks, which never fire before the
// transport has data. Both libgit2 options default to 0 (= OS default).
unsafe {
    git2::opts::set_server_connect_timeout_in_milliseconds(CONNECT_TIMEOUT_MS)?;
    git2::opts::set_server_timeout_in_milliseconds((cfg.network_timeout_secs * 1000) as i32)?;
}
```

### 7.2 Shared preamble

```rust
fn open_tree(ctx: &OpCtx) -> Result<Repository, GitError> {
    let path = ctx.tree.clone();                       // repos_dir.join(validated id)
    if !path.exists() { return Err(GitError::no_worktree(&ctx.def.id)); }
    let repo = Repository::open(&path)?;
    // Re-point the remote if the definition changed since the last operation.
    if let Some(url) = ctx.def.remote.as_deref() {
        match repo.find_remote(&ctx.def.remote_name) {
            Ok(r) if r.url() != Some(url) => { repo.remote_set_url(&ctx.def.remote_name, url)?; }
            Ok(_)  => {}
            Err(_) => { repo.remote(&ctx.def.remote_name, url)?; }
        }
    }
    Ok(repo)
}

/// Mutating ops refuse to guess which branch the caller meant.
fn require_branch(repo: &Repository, branch: &str) -> Result<(), GitError> {
    if repo.is_empty()? { return Ok(()); }                       // unborn is fine
    if repo.head_detached()? { return Err(GitError::detached_head()); }
    let head = repo.head()?;
    let short = head.shorthand()?;                               // 0.21: Result, not Option
    if short != branch { return Err(GitError::branch_mismatch(short, branch)); }
    Ok(())
}
```

Non-UTF-8 index paths are legal in git and `String::from_utf8_lossy` produces a path that no longer addresses the entry:

```rust
#[cfg(unix)]
fn bytes_to_path(b: &[u8]) -> Result<PathBuf, GitError> {
    use std::os::unix::ffi::OsStrExt;
    Ok(PathBuf::from(std::ffi::OsStr::from_bytes(b)))
}
#[cfg(windows)]
fn bytes_to_path(b: &[u8]) -> Result<GitError, GitError> {
    std::str::from_utf8(b).map(PathBuf::from)
        .map_err(|_| GitError::io("non-UTF-8 path in index is not representable on Windows"))
}
```

### 7.3 `init`

```rust
// Repository::init honours the developer's global init.defaultBranch, so an
// unpinned init produces refs/heads/master on half of all machines and every
// later refspec (refs/heads/<def.branch>) silently fails to match.
std::fs::create_dir_all(&ctx.tree)?;
let mut o = git2::RepositoryInitOptions::new();
o.initial_head(&ctx.def.branch).no_reinit(true);
let repo = match Repository::init_opts(&ctx.tree, &o) {
    Ok(r) => r,
    Err(e) if e.code() == ErrorCode::Exists => Repository::open(&ctx.tree)?,  // idempotent
    Err(e) => return Err(e.into()),
};
if let Some(url) = ctx.def.remote.as_deref() {
    if repo.find_remote(&ctx.def.remote_name).is_err() { repo.remote(&ctx.def.remote_name, url)?; }
}
// outcome: "initialized"
```

### 7.4 `clone`

```rust
if ctx.tree.exists() && std::fs::read_dir(&ctx.tree)?.next().is_some() {
    // Idempotent: an existing repo is reused; a non-empty non-repo is refused.
    return match Repository::open(&ctx.tree) {
        Ok(_)  => Ok(OpOutcome::already_cloned()),
        Err(_) => Err(GitError::not_a_repository(&ctx.tree)),
    };
}
let url = ctx.def.remote.as_deref().ok_or_else(GitError::remote_missing)?;
let mut fo = git2::FetchOptions::new();
fo.remote_callbacks(creds::callbacks(ctx));
ctx.phase(Phase::Cloning);
// Repository::clone (the free function) takes no callbacks and cannot authenticate.
let repo = git2::build::RepoBuilder::new().fetch_options(fo).clone(url, &ctx.tree)?;
ensure_branch(&repo, &ctx.def)?;      // see below
// outcome: "cloned"
```

```rust
/// After a clone the remote's default branch may not be `def.branch`.
fn ensure_branch(repo: &Repository, def: &RepoDef) -> Result<(), GitError> {
    if repo.head().ok().and_then(|h| h.shorthand().ok().map(str::to_owned))
           .as_deref() == Some(def.branch.as_str()) { return Ok(()); }
    let tracking = format!("refs/remotes/{}/{}", def.remote_name, def.branch);
    let target = match repo.find_reference(&tracking) {
        Ok(r) => r.target().ok_or_else(|| GitError::internal("remote ref has no target"))?,
        Err(_) => repo.head()?.peel_to_commit()?.id(),      // create from current HEAD
    };
    let commit = repo.find_commit(target)?;
    let mut b = repo.branch(&def.branch, &commit, true)?;
    if repo.find_reference(&tracking).is_ok() {
        b.set_upstream(Some(&format!("{}/{}", def.remote_name, def.branch)))?;
    }
    let obj = repo.revparse_single(&format!("refs/heads/{}", def.branch))?;
    // Order matters and is API-documented: checkout_tree THEN set_head.
    let mut co = git2::build::CheckoutBuilder::new(); co.force();
    repo.checkout_tree(&obj, Some(&mut co))?;
    repo.set_head(&format!("refs/heads/{}", def.branch))?;
    Ok(())
}
```

### 7.5 `commit` / stage-all

```rust
ctx.phase(Phase::Staging);
let mut index = repo.index()?;
let specs: Vec<&str> = ctx.request.paths.as_deref()
    .map(|v| v.iter().map(String::as_str).collect())
    .unwrap_or_else(|| vec!["*"]);
// Verified: add_all(["*"], DEFAULT) is `git add -A` — it stages deletions of tracked
// files AND brand-new untracked files. update_all would miss the new ones, so it is
// not needed. DEFAULT honours .gitignore; IndexAddOption::FORCE is never used here,
// because a generic executor that bypasses .gitignore will commit .env.
index.add_all(specs.iter(), git2::IndexAddOption::DEFAULT, None)?;
index.write()?;

let tree_oid = index.write_tree()?;
let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
let changed = parent.as_ref().map_or(true, |p| p.tree_id() != tree_oid);
let mut committed = None;
if changed || ctx.request.allow_empty {
    ctx.phase(Phase::Committing);
    let tree = repo.find_tree(tree_oid)?;
    // NEVER repo.signature(): verified to fail with
    // code=NotFound class=Config "config value 'user.name' was not found".
    let sig = git2::Signature::now(&ctx.author_name(), &ctx.author_email())?;
    let msg = ctx.request.message.clone().unwrap_or_else(|| format!(
        "{}: sync from {} at {}", ctx.identity.app_name, ctx.identity.hostname,
        utc_stamp(now_ms())));
    let parents: Vec<&git2::Commit> = parent.iter().collect();   // empty slice == root commit
    committed = Some(repo.commit(Some("HEAD"), &sig, &sig, &msg, &tree, &parents)?);
}
```

Author precedence: request `author` → `def.author` → `[git].author_*` → `[app].name` / `<identifier>@<hostname>`.

### 7.6 `pull` / `sync` — fetch, then the prefer-local merge

`sync` = `[settings copy] → commit-all → fetch → merge → [settings write-back] → push`.
`pull` = `fetch → merge` only; if the tree is dirty and `commit_local` is false it fails fast with `dirty_tree` rather than silently committing on the caller's behalf. `commit_local: true` makes `pull ≡ sync` with `push: false`.

Committing everything **before** the fetch is load-bearing, not merely convenient: after that step every tracked file in the working tree is at a committed state, which is what makes the later `CheckoutBuilder::force()` provably non-destructive, and it collapses "HEAD is unborn" to "the tree really was empty".

**Fetch**

```rust
ctx.phase(Phase::Fetching);
let mut remote = repo.find_remote(&ctx.def.remote_name)?;
let refspec = format!("+refs/heads/{b}:refs/remotes/{r}/{b}",
                      b = ctx.def.branch, r = ctx.def.remote_name);
let mut fo = git2::FetchOptions::new();
fo.remote_callbacks(creds::callbacks(ctx));
fo.download_tags(git2::AutotagOption::None);           // tags are out of scope (§13)
remote.fetch(&[&refspec], Some(&mut fo), Some("host sync"))?;
```

**Analyse** — a remote that has no such branch yet is *not* an error; push will create it.

```rust
let tracking = format!("refs/remotes/{}/{}", ctx.def.remote_name, ctx.def.branch);
let their_ref = match repo.find_reference(&tracking) {
    Ok(r) => r,
    Err(e) if e.code() == ErrorCode::NotFound => return Ok(MergeOutcome::NoRemoteBranch),
    Err(e) => return Err(e.into()),
};
let their = repo.reference_to_annotated_commit(&their_ref)?;
let (analysis, _pref) = repo.merge_analysis(&[&their])?;
```

Branch **in this order** — a fast-forward situation sets `ANALYSIS_NORMAL | ANALYSIS_FASTFORWARD`, so testing `is_normal()` first turns every fast-forward into a spurious merge commit:

| # | test | action | outcome |
|---|---|---|---|
| 1 | `analysis.is_up_to_date()` | nothing | `up_to_date` |
| 2 | `analysis.is_unborn()` | adopt remote | `fast_forward` |
| 3 | `analysis.is_fast_forward()` | move ref + force checkout | `fast_forward` |
| 4 | `analysis.is_normal()` | prefer-local merge | `merged` / `merged_unrelated` |

```rust
// 2. UNBORN — reachable only when the tree was genuinely empty (see above).
let refname = format!("refs/heads/{}", ctx.def.branch);
repo.reference(&refname, their.id(), true, "sync: adopt remote history")?;
repo.set_head(&refname)?;
repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))?;

// 3. FAST-FORWARD — Repository::merge() does NOT fast-forward and does NOT move
//    HEAD (verified); it also leaves RepositoryState::Merge. Do the ref surgery.
let mut r = repo.find_reference(&refname)?;
r.set_target(their.id(), "sync: fast-forward")?;
repo.set_head(&refname)?;
repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))?;
```

**4. The prefer-local merge** (`merge.rs`):

```rust
pub fn merge_prefer_local(repo: &Repository, their_oid: Oid, ctx: &OpCtx)
    -> Result<Option<MergeResult>, GitError>
{
    ctx.phase(Phase::Merging);
    let ours   = repo.head()?.peel_to_commit()?;
    let theirs = repo.find_commit(their_oid)?;

    // Default FileFavor::Normal, NOT FileFavor::Ours. `Ours` resolves at *hunk*
    // granularity, splicing the two sides together inside one file — that is not
    // "whole-file last-writer-wins preferring local". With Normal, non-overlapping
    // edits to the same file still auto-merge cleanly, and prefer-local applies
    // only where the sides genuinely collide. (`Ours` also leaves every tree-level
    // conflict unresolved anyway, so it buys nothing here.)
    let opts = git2::MergeOptions::new();

    let unrelated = matches!(repo.merge_base(ours.id(), theirs.id()),
                             Err(ref e) if e.code() == ErrorCode::NotFound);
    let mut idx: git2::Index = if unrelated {
        // Local `init` + commit against a remote with its own root commit: no merge
        // base exists. Two-way prefer-local is well defined against an empty ancestor.
        // Verified against the 0.21.0 source: treebuilder(Option<&Tree>) -> TreeBuilder,
        // and merge_trees(ancestor, ours, theirs, Option<&MergeOptions>) -> Index.
        let empty = repo.find_tree(repo.treebuilder(None)?.write()?)?;
        repo.merge_trees(&empty, &ours.tree()?, &theirs.tree()?, Some(&opts))?
    } else {
        repo.merge_commits(&ours, &theirs, Some(&opts))?    // note: &, not &mut
    };

    let mut resolved: Vec<String> = Vec::new();
    if idx.has_conflicts() {
        let conflicts: Vec<git2::IndexConflict> = idx.conflicts()?.collect::<Result<_, _>>()?;

        // ---- dir/file guard -------------------------------------------------
        // Verified hole: local file `x` vs remote directory `x/inner.txt` yields an
        // index with x@stage2 and x/inner.txt@stage0. Prefer-ours resolution then
        // WRITES A TREE SUCCESSFULLY in which the remote directory wins and the
        // local file is silently gone, with a clean statuses() afterwards. Refuse.
        let our_tree = ours.tree()?; let their_tree = theirs.tree()?;
        for c in &conflicts {
            let raw = conflict_path_bytes(c)?;
            let p = bytes_to_path(&raw)?;
            let ours_kind   = our_tree.get_path(&p).ok().and_then(|e| e.kind());
            let theirs_kind = their_tree.get_path(&p).ok().and_then(|e| e.kind());
            if let (Some(a), Some(b)) = (ours_kind, theirs_kind) {
                if a != b {
                    return Err(GitError::merge_path_type_conflict(&p));
                }
            }
        }

        // ---- whole-file prefer-local ---------------------------------------
        for c in conflicts {
            let raw = conflict_path_bytes(&c)?;
            let path = bytes_to_path(&raw)?;
            // Drops all three stages for this path. IndexEntry is NOT Clone, so the
            // `our` entry is MOVED out of the conflict, not cloned.
            idx.remove_path(&path)?;
            match c.our {
                Some(mut e) => { e.flags = 0; e.flags_extended = 0; idx.add(&e)?; } // stage 0
                None => { /* deleted locally, modified remotely -> stays deleted */ }
            }
            resolved.push(path.to_string_lossy().into_owned());
        }
    }
    if idx.has_conflicts() { return Err(GitError::merge_unresolvable()); }

    let tree_oid = idx.write_tree_to(repo)?;      // in-memory index -> _to(repo)
    if tree_oid == ours.tree_id() && repo.graph_descendant_of(ours.id(), their_oid)? {
        return Ok(None);                          // reliable "nothing to do"
    }

    let sig = git2::Signature::now(&ctx.author_name(), &ctx.author_email())?;
    let msg = format!(
        "Merge {r}/{b} into {b}\n\n\
         Automatic two-way sync by {app} on {host} at {ts}.\n\
         Conflicting files were resolved in favour of the local copy:\n{list}\n\n\
         The remote version of each file above is preserved as the second parent:\n\
         git show <this-commit>^2:<path>",
        r = ctx.def.remote_name, b = ctx.def.branch,
        app = ctx.identity.app_name, host = ctx.identity.hostname, ts = utc_stamp(now_ms()),
        list = if resolved.is_empty() { "  (none)".into() }
               else { resolved.iter().map(|p| format!("  {p}")).collect::<Vec<_>>().join("\n") });

    // >>> THE MERGE COMMIT <<<  parent 0 = ours, parent 1 = theirs.
    // Some("HEAD") advances the branch ref and leaves HEAD attached (verified twice).
    let tree = repo.find_tree(tree_oid)?;
    let merge_oid = repo.commit(Some("HEAD"), &sig, &sig, &msg, &tree, &[&ours, &theirs])?;

    ctx.phase(Phase::CheckingOut);
    repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))?;
    let _ = repo.cleanup_state();                 // defensive; merge_commits sets no MERGE_HEAD
    Ok(Some(MergeResult { merge_commit: merge_oid, conflicts_resolved: resolved, unrelated }))
}
```

**Why `merge_commits`/`merge_trees` and never `Repository::merge`.** `Repository::merge` writes into the repository's index, sets `MERGE_HEAD`/`MERGE_MSG`, checks out immediately, and leaves `RepositoryState::Merge` until `cleanup_state()`. `merge_commits` is *pure*: it returns a detached in-memory `Index` and touches nothing on disk. Consequences, all of them the point:

- Every failure path before the `commit` line is a **no-op on disk**. A crash mid-merge leaves HEAD at the pre-merge commit with no merge state; the next sync just redoes it.
- Conflict markers can never reach the working tree, even transiently, because the only thing ever checked out is a tree built from an index with `has_conflicts() == false` — and with `FileFavor::Normal` plus explicit resolution, libgit2 is never asked to synthesise marker content in the first place.
- There is no "resume the merge" state for a human or a later job to reason about.

`repo.checkout_head(force)` is correct *here* because the merge tree was computed from `ours`, which is exactly what the commit-all step wrote; forcing cannot destroy uncommitted tracked work, and `force` alone does not remove untracked files (`remove_untracked` is not set).

### 7.7 `push`

```rust
ctx.phase(Phase::Pushing);
let spec = if ctx.request.force { format!("+refs/heads/{b}:refs/heads/{b}", b = branch) }
           else                 { format!( "refs/heads/{b}:refs/heads/{b}", b = branch) };
let mut po = git2::PushOptions::new();
po.remote_callbacks(creds::callbacks(ctx));            // includes push_update_reference
remote.push(&[&spec], Some(&mut po))?;
ctx.take_push_rejections()?;                           // -> push_rejected / not_fast_forward
```

Two disjoint rejection paths, both required:

- **Non-fast-forward is caught client-side, before any server round trip.** `push()` returns `Err(code=NotFastForward, class=Reference)` and **`push_update_reference` is never invoked**. So `push()` returning `Err` is genuinely sufficient for the ordinary "someone else pushed first" case.
- **Policy rejections** (branch protection, `pre-receive` hooks) are invisible to libgit2 locally; they arrive *only* through `push_update_reference(refname, Some(status))`. Our callback records them and `take_push_rejections` turns a non-empty set into `push_rejected`. Without it, `Remote::push` returns `Ok(())` while the server refused the ref and the job reports a success that pushed nothing.

The leading `+` comes *solely* from the explicit `force: true` request field — never from config, never as a retry fallback after a rejection. Force-with-lease is **not supported** by git2 0.21 (`PushOptions` has no expected-old-oid parameter); the fetch-compare-then-force approximation is a TOCTOU window, not a lease, and is cut (§13).

**Upstream after the first push.** Clone sets upstream automatically; a manual push does **not** (verified). Without this, a repo the host created by `init` + `push` has no `branch.<b>.merge`, and every later `graph_ahead_behind` via `Branch::upstream()` fails with `NotFound` forever:

```rust
let mut b = repo.find_branch(&branch, git2::BranchType::Local)?;
if b.upstream().is_err() {
    b.set_upstream(Some(&format!("{}/{}", ctx.def.remote_name, branch)))?;
    outcome.upstream_set = true;
}
```

Pushing an empty branch fails with `src refspec '…' does not match any existing object` — guard with `repo.is_empty()?` and report `unborn_branch`.

### 7.8 `status` and `branches`

```rust
let mut so = git2::StatusOptions::new();
so.include_untracked(true).recurse_untracked_dirs(true)
  .include_ignored(false).include_unmodified(false)
  .renames_head_to_index(true).renames_index_to_workdir(true);
let sts = repo.statuses(Some(&mut so))?;
// bucket by Status bits, cap each bucket at MAX_DIRTY_FILES, set truncated

let (ahead, behind) = match (repo.head().ok().and_then(|h| h.target()),
                             repo.find_branch(&branch, BranchType::Local).ok()
                                 .and_then(|b| b.upstream().ok())
                                 .and_then(|u| u.get().target())) {
    (Some(l), Some(u)) => repo.graph_ahead_behind(l, u)?,
    _ => (0, 0),
};
```

`branches` uses `repo.branches(Some(BranchType::Local))` / `Remote`, `Branch::name() -> Result<Option<&str>>`, `Branch::is_head()`, `Branch::upstream()`.

### 7.9 `branch`

```rust
if body.create {
    let from = match &body.from {
        Some(rev) => repo.revparse_single(rev)?.peel_to_commit()?,
        None      => repo.head()?.peel_to_commit()?,
    };
    repo.branch(&body.name, &from, false)?;      // force=false: 409 already_exists on clash
}
if body.checkout {
    let obj = repo.revparse_single(&format!("refs/heads/{}", body.name))?;
    let mut co = git2::build::CheckoutBuilder::new();
    co.safe();                                   // a dirty tree must block, not be clobbered
    repo.checkout_tree(&obj, Some(&mut co))
        .map_err(|e| GitError::from(e).as_dirty_tree_if_conflict())?;
    repo.set_head(&format!("refs/heads/{}", body.name))?;   // AFTER checkout_tree
}
if let Some(up) = &body.upstream {
    repo.find_branch(&body.name, BranchType::Local)?.set_upstream(up.as_deref())?;  // None unsets
}
```

### 7.10 `reset`

```rust
let target = match body.to.as_deref().unwrap_or("head") {
    "head"     => repo.head()?.peel(ObjectType::Commit)?,
    "upstream" => repo.find_branch(&branch, BranchType::Local)?.upstream()?
                      .get().peel(ObjectType::Commit)?,
    rev        => repo.revparse_single(rev)?.peel(ObjectType::Commit)?,
};
// libgit2 hard-codes opts.checkout_strategy = GIT_CHECKOUT_FORCE for a hard reset
// (reset.c:152), silently discarding ANY CheckoutBuilder strategy passed here.
// Passing one is a no-op, so pass None and do the untracked cleanup separately.
repo.reset(&target, git2::ResetType::Hard, None)?;

let mut co = git2::build::CheckoutBuilder::new();
co.force();
if body.clean_untracked { co.remove_untracked(true); }
// remove_ignored is NEVER set: it deletes .env-style files, and a "discard local"
// button over HTTP must not be able to do that.
repo.checkout_head(Some(&mut co))?;
```

### 7.11 `purge`

```rust
fn purge_tree(repos_root_canon: &Path, tree: &Path) -> Result<(), GitError> {
    if std::fs::symlink_metadata(tree)?.file_type().is_symlink() {
        return Err(GitError::path_refused(tree, "is a symlink"));
    }
    let real = std::fs::canonicalize(tree)?;          // resolves symlinks anywhere in the chain
    if real == *repos_root_canon || !real.starts_with(repos_root_canon) {
        return Err(GitError::path_refused(&real, "resolves outside the repos root"));
    }
    std::fs::remove_dir_all(&real).map_err(GitError::from)
}
```

Two independent barriers: the id charset (no `/`, no `..`, no absolute path expressible) and canonicalized containment (catches a pre-existing symlink at `repos/<id>`). Refusals are `403 path_refused` and are logged.

---

## 8. Credentials

### 8.1 Resolution order

```rust
pub enum ResolvedCred {
    None,
    Token   { username: String, token: Secret },
    SshPath { username: String, public: Option<PathBuf>, private: PathBuf, pass: Option<Secret> },
    SshMem  { username: String, public: Option<String>,  private: Secret,  pass: Option<Secret> },
}
pub fn resolve(per_call: Option<&CredentialSpec>, stored: Option<&CredentialSpec>,
               remote: Option<&str>) -> Result<ResolvedCred, GitError>;
```

1. A `credential` in the request body **wholly replaces** the stored value — never a field-by-field merge. Merging produces hybrids nobody asked for (a per-call PAT silently combined with a stored SSH passphrase) and needs a paragraph of rules; replacement needs one sentence. A per-call `{"kind":"none"}` explicitly disables a stored credential for one operation (useful for public HTTPS pulls that shouldn't spend a rate-limited PAT).
2. Otherwise the repo's stored credential — **subject to `bound_host`**.
3. Otherwise `None` — correct for public HTTPS, for `file://` remotes (the whole test suite), and for local-only repos.

**Per-call credentials are never written to `repos.json`.** To persist, `PUT` the repo. (A `"store": true` flag was considered and cut: it exactly duplicates `PUT`.)

### 8.2 `bound_host`

Every stored credential records `bound_host`, the lowercased host of `remote` at the moment it was stored. It is only ever sent to that host.

- A `PUT` whose `remote` has a different host than the stored credential's `bound_host`, and which supplies no new credential, **drops the stored credential** and returns `warnings: [{code:"auth_unbound", …}]`.
- An operation that would need an unbound credential does **not** transmit it: it proceeds as `None` and, if the remote demands auth, fails `auth_unbound` with a message saying exactly what to do.

This closes the single worst new primitive the feature creates: a loopback caller repointing a repo at an attacker host, triggering a fetch, and receiving the user's PAT.

### 8.3 Callback wiring

```rust
pub fn callbacks<'a>(ctx: &'a OpCtx) -> git2::RemoteCallbacks<'a> {
    // Each RemoteCallbacks setter stores its own Box<dyn FnMut + 'a>, so two
    // closures cannot both capture the same &mut. Share by shared reference.
    let attempts: &'a Cell<u32> = &ctx.cred_attempts;
    let mut cb = git2::RemoteCallbacks::new();

    cb.credentials(move |_url, username_from_url, allowed| {
        // libgit2 asks for USERNAME alone before every SSH auth attempt. It always
        // precedes the real ask, so it must not consume the budget or a single SSH
        // connect would burn the whole allowance. (scp-style git@host:path skips
        // this phase entirely — handle both.)
        if allowed.contains(git2::CredentialType::USERNAME) {
            return git2::Cred::username(ctx.cred.username().unwrap_or("git"));
        }
        let n = attempts.get() + 1;
        attempts.set(n);
        // Verified: libgit2 re-invokes this up to 15 times before giving up
        // ("too many redirects or authentication replays"). Self-limit.
        if n > MAX_CRED_ATTEMPTS {
            return Err(git2::Error::from_str("authentication failed: credentials rejected by remote"));
        }
        match (&ctx.cred, allowed) {
            (ResolvedCred::Token { username, token }, a)
                if a.contains(git2::CredentialType::USER_PASS_PLAINTEXT) =>
                    git2::Cred::userpass_plaintext(username, token.expose()),
            (ResolvedCred::SshMem { username, public, private, pass }, a)
                if a.contains(git2::CredentialType::SSH_MEMORY)
                || a.contains(git2::CredentialType::SSH_KEY) =>
                    git2::Cred::ssh_key_from_memory(username, public.as_deref(),
                        private.expose(), pass.as_ref().map(Secret::expose)),
            (ResolvedCred::SshPath { username, public, private, pass }, a)
                if a.contains(git2::CredentialType::SSH_KEY) =>
                    git2::Cred::ssh_key(username, public.as_deref(), private,
                        pass.as_ref().map(Secret::expose)),
            // Fail NOW rather than falling through to Cred::default(), which is the
            // branch that loops forever with the same rejected identity.
            (_, a) => Err(git2::Error::from_str(&ctx.auth_missing_message(a))),
        }
    });

    cb.transfer_progress(|p| ctx.transfer(&p));       // false => abort
    cb.sideband_progress(|_| !ctx.aborted());
    cb.push_update_reference(|refname, status| { ctx.record_push_status(refname, status); Ok(()) });
    cb.certificate_check(|cert, host| ctx.host_key.check(cert, host));
    cb
}
```

`allowed` is a **bitmask — test with `.contains()`, never `==`.** Our own error message propagates verbatim into `git2::Error::message()`, so it becomes the job's `error.message`.

**No ambient fallback, ever.** The host never calls `Cred::default()` (Windows NTLM/Negotiate against the ambient session), `Cred::ssh_key_from_agent()`, or `Cred::credential_helper()`. Falling back to the user's agent or `~/.gitconfig` helper would let a loopback caller push to *any* repo the user's personal credentials reach — the host would be a credential-laundering proxy for the user's whole GitHub account — and it would make failures machine-dependent. This is the most important code in `creds.rs` and it is code that does *not* exist. (Alternative: a `kind: "ssh_agent"` variant, ~15 LOC, add only on request.)

### 8.4 SSH host keys — TOFU

With no `certificate_check` callback, libssh2 accepts **any** host key: a silent MITM on every SSH sync. Under `ssh_host_key_policy = "tofu"` (default):

```rust
fn check(&self, cert: &git2::Cert<'_>, host: &str)
    -> Result<git2::CertificateCheckStatus, git2::Error>
{
    // All three names verified against the 0.21.0 source: Cert::as_hostkey()
    // -> Option<&CertHostkey>, CertHostkey::hash_sha256() -> Option<&[u8; 32]>,
    // and CertificateCheckStatus::CertificatePassthrough.
    let Some(hk) = cert.as_hostkey() else {
        // A TLS certificate: libgit2 has ALREADY verified it. Returning
        // CertificateOk here would BYPASS that verification — pass through instead.
        return Ok(git2::CertificateCheckStatus::CertificatePassthrough);
    };
    if self.policy == SshHostKeyPolicy::Accept {
        return Ok(git2::CertificateCheckStatus::CertificateOk);
    }
    let Some(sha) = hk.hash_sha256() else {
        return Err(git2::Error::from_str("remote offered no SHA-256 host key hash"));
    };
    let fp = format!("SHA256:{}", b64_nopad(sha));
    match &self.pinned {
        None => { self.learned.set(Some(fp)); Ok(git2::CertificateCheckStatus::CertificateOk) }
        Some(p) if *p == fp => Ok(git2::CertificateCheckStatus::CertificateOk),
        Some(p) => Err(git2::Error::new(git2::ErrorCode::Certificate, git2::ErrorClass::Ssh,
            &format!("host key for {host} changed (pinned {p}, got {fp}); clear \
                      ssh_host_fingerprint for this repo in git-state.json to re-pin"))),
    }
}
```

A learned fingerprint travels back with the job outcome and is persisted by `GitService::after_job` into `git-state.json` — never by the blocking thread, never into `repos.json`.

**TLS verification is never bypassable.** There is no `insecure_tls` flag anywhere in the API or the config, and adding one is refused (§13).

### 8.5 Keeping secrets out of everything

| sink | mechanism |
|---|---|
| GET responses | `Secret` has no `Serialize`; `RepoView`/`CredentialView` have no secret-shaped fields. Two structural barriers, zero conventions to remember. |
| URLs | A `remote` with a userinfo password is rejected at define time (422 `insecure_remote`) — libgit2 quotes URLs into its error strings, which land in job records, `git.log`, and dialogs. |
| Error messages | `scrub(msg, &cred)` runs on every `GitError` before it reaches a job record, `git-state.json`, `git.log`, or a dialog: strip `scheme://user:pw@` → `scheme://`, then literal-replace every exposed secret longer than 6 chars with `***`. |
| `Debug` | `Secret`'s `Debug` prints `Secret(***)`; `RepoDef`'s is derived on top of it. |
| Process env / argv | libgit2 is a **library**. No `git` subprocess ever runs, so no credential appears in argv or a child's environment. |
| Credential helpers | Never invoked; `~/.git-credentials` and `osxkeychain`/`manager-core` are never read. |
| On disk | `repos.json` at `0600` on unix, written via a `0600` temp file + rename. |

An auditor greps for `\.expose()` and must find exactly four call sites, all inside the `cb.credentials` closure.

`git.log` records one line per job:
`<epoch_ms> <op> repo=<id> job=<id> ok|err code=<code> <scrubbed message>`, opened through `supervisor::open_log(path, LOG_MAX_BYTES)` so it rotates at 5 MB to `git.log.old` exactly like `host.log`, `server.log`, and `chrome.log`.

---

## 9. Host integration

### 9.1 The one rule

> The git subsystem never touches `App`, `Children`, `chrome_generation`, `server_generation`, `rt.enter()`, or `tokio::process`. Its only channel into the tao loop is `HostEvent`, which already exists and is already bridged.

`GitService` holds `paths`, `cfg`, the settings schema, the runtime handle, and the event sender — deliberately **not** `Arc<Mutex<Children>>`, not `status`, not the generation atomics. That is the invariant that makes the feature safe, and it gets a comment on the struct.

No new `rt.enter()` site is created because git spawns no OS processes: `Handle::spawn` and `Handle::spawn_blocking` are callable from any thread without a guard. (`rt.enter()` exists in this codebase solely because `tokio::process::Command::spawn` registers a child with the runtime's signal driver.)

### 9.2 Child environment

```rust
pub struct HostAccess<'a> { pub url: &'a str, pub token: &'a str, pub git_enabled: bool }

pub fn build_env(cfg_env: &BTreeMap<String, String>, settings_env: &[(String, String)],
                 settings_file: &Path, host: &HostAccess<'_>) -> Vec<(String, String)>
```

adds `APP_HOST_URL = http://127.0.0.1:<port>`, `APP_HOST_TOKEN = <token>`, and `APP_GIT_ENABLED = "1"` when `[git]` is present. This vec is used **only** for the `[server]` child; `chrome::launch` inherits the host's own environment, which never contains the token.

### 9.3 New `HostEvent` variants

```rust
pub enum HostEvent {
    RestartRequested,
    /// Git wants the children restarted (a pull moved HEAD, or settings changed).
    /// This MUST travel the channel rather than calling App::restart() directly:
    /// restart() -> kill_children() -> rt.block_on(), and block_on panics when
    /// called from inside a runtime worker thread. The hop is load-bearing.
    GitRestartChildren { repo_id: String, reason: &'static str },   // "settings" | "requested"
    GitFailed { repo_id: String, op: String, code: String, message: String },
    /// A sync succeeded but resolved conflicts in favour of the local copy —
    /// i.e. it overwrote someone else's edit. Carries the merge commit so the
    /// arm can de-duplicate: one data-loss event, at most one dialog, ever.
    GitConflictsResolved { repo_id: String, merge_commit: String, paths: Vec<String> },
}
```

Event-loop arms (in `main.rs`, both dumb consumers — all policy was decided in `src/git/`):

```rust
Event::UserEvent(UserEvent::Host(HostEvent::GitRestartChildren { repo_id, reason })) => {
    // A background sync must never steal focus from a user who deliberately
    // minimised the app, and must not fight a restart that is already in flight.
    if app.in_tray_mode {
        app.log(&format!("git[{repo_id}]: restart suppressed ({reason}) — in tray mode"));
    } else if app.git_restart_ok() {                 // RESTART_DEBOUNCE_MS = 2000
        app.log(&format!("git[{repo_id}]: restarting children ({reason})"));
        app.restart();
        app.refresh_tray(&mut tray_handle, app.in_tray_mode);
    }
}
Event::UserEvent(UserEvent::Host(HostEvent::GitFailed { repo_id, op, code, message })) => {
    app.log(&format!("git[{repo_id}]: {op} failed [{code}]: {message}"));
    if app.cfg.git.as_ref().is_some_and(|g| g.error_dialogs) && !app.in_tray_mode {
        dialog(&app.cfg.app.name,
               &format!("Git {op} failed for \"{repo_id}\" ({code}).\n\n{message}\n\nLog: {}",
                        app.paths.logs_dir.join("git.log").display()));
    }
}
```

**`GitFailed` is emitted only on an ok→fail transition** (previous `last_sync.ok` was `true` or absent). `dialog()` is modal and blocks the tao loop — including menu clicks and `ChromeExited` handling — so a repo whose token expired with `auto_sync_secs = 300` would otherwise stack a modal every five minutes forever, with the user unable to reach the tray to turn it off. The transition test fires exactly once per outage and needs no cooldown map.

**`GitConflictsResolved` — the lossy-success notification.** A prefer-local merge that resolved at least one conflict has, by definition, overwritten an edit made somewhere else. The user is told, under the same `error_dialogs` boolean:

```rust
Event::UserEvent(UserEvent::Host(HostEvent::GitConflictsResolved { repo_id, merge_commit, paths })) => {
    app.log(&format!("git[{repo_id}]: {} file(s) resolved in favour of the local copy \
                      in {merge_commit}: {}", paths.len(), paths.join(", ")));
    // De-duplicated on the merge commit id: a push that fails and is retried
    // re-reports the SAME merge, and one overwrite must not produce two dialogs.
    // Bounded set (last 16 ids), so it cannot grow without limit.
    if app.cfg.git.as_ref().is_some_and(|g| g.error_dialogs)
        && !app.in_tray_mode
        && app.git_notice_is_new(&merge_commit)
    {
        notice(&app.cfg.app.name, &format!(
            "\"{repo_id}\" synced, but {n} file(s) had been changed both here and \
             remotely. The local version was kept:\n\n{list}\n\nThe remote version is \
             not lost — recover it with:\n  git show {merge_commit}^2:<path>\n\nLog: {log}",
            n = paths.len(),
            list = paths.iter().take(10).map(|p| format!("  {p}")).collect::<Vec<_>>().join("\n"),
            log = app.paths.logs_dir.join("git.log").display()));
    }
}
```

Emitted by `after_job` whenever `result.merge.conflicts_resolved` is non-empty — including for auto-syncs, which is the case that most needs it, since nobody was watching. Three guards make the modal safe: **tray-mode suppression** (identical to `GitFailed` — a background sync must not steal focus from a user who deliberately minimised the app), **merge-commit de-duplication** (a retried push re-reports one merge; the user sees one dialog), and a **10-path cap** in the body so a large merge cannot produce an unreadable modal. It never blocks the sync — the job had already succeeded before the event was sent.

`notice()` is a sibling of the existing `dialog()` in `main.rs`, identical but for `MessageLevel::Warning` instead of `Error`: this is a successful sync reporting a consequence, and styling it as an error would misrepresent what happened and train the user to dismiss the failure dialogs too.

```rust
fn notice(title: &str, description: &str) {
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Warning)
        .set_title(title)
        .set_description(description)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}
```

*(This overrides the recommendation this document originally carried — that a conflict-resolved sync stay silent because "git is invisible by default". The counter-argument that won: silent data loss is the one thing worth breaking that rule for, and `error_dialogs` is off by default anyway, so an app that wants git invisible still gets it.)*

### 9.4 Auto triggers

**Interval.** One `rt.spawn` task per repo with `auto_sync_secs > 0`, started in `GitService::start` and re-spawned on `PUT`:

```rust
// A sleep loop rather than tokio::time::interval, so the period is re-read from the
// registry each cycle: a PUT that changes or clears auto_sync_secs takes effect after
// at most one old period, and a DELETE lets the task exit on its own. MissedTickBehavior
// is moot here — a sleep loop cannot burst after a laptop resume.
rt.spawn(async move {
    loop {
        let Some(secs) = svc.auto_sync_secs(&id) else { break };
        tokio::time::sleep(Duration::from_secs(secs)).await;
        if svc.draining() { break }
        match svc.start_job(&id, JobOp::Sync, OpRequest::auto()) {
            Ok(Admission::Busy(j)) => svc.log(&format!("git[{id}]: auto-sync skipped, {} running", j.id)),
            Ok(_) => {}
            Err(e) => svc.log_err(&id, &e),
        }
    }
    svc.timers().remove(&id);
});
```

A tick on a busy repo is **dropped, not queued** — a backlog of stale syncs is never useful and would turn one slow network into an unbounded queue. `timers: Mutex<BTreeSet<String>>` prevents double-spawning.

**`OpRequest::auto()` hard-codes `restart_children: false`**, and there is no config key that can turn it on. Only an explicit HTTP call, or a real `sync_settings` change, can restart the user's window.

**`sync_on_start`** fires from `Event::NewEvents(StartCause::Init)`, **after** `app.start_children()`, as a fire-and-forget `Handle::spawn`. It returns instantly and never blocks the loop. Sequencing after `start_children` is deliberate on both counts: the child server is already up, so a `sync_settings` pull restarts a live child rather than racing the first spawn, and a wedged network at launch cannot make the app look dead before the first window appears.

**`sync_on_quit`** runs inside `App::quit()` after `kill_children()` (§6.6).

### 9.5 Tray

```rust
pub fn menu_model(menu: &MenuSection, app_name: &str, settings_enabled: bool,
                  show_open: bool, show_sync: bool) -> Vec<Entry>
```

inserts `Entry::Action(TrayAction::GitSync, "Sync now")` immediately before `"Restart App"`. `refresh_tray` passes `show_sync = cfg.git.as_ref().is_some_and(|g| g.tray_sync) && git.repo_count() > 0` — the entry does not appear when there is nothing to sync. Handler:

```rust
Some(tray::TrayAction::GitSync) => {
    if let Some(git) = &app.git { git.sync_all_manual(); }   // admits, returns instantly
}
```

`sync_all_manual` admits one `sync` per repo, skips busy repos with a log line, and never blocks. No tray refresh (the menu did not change); busy state is deliberately not reflected in the menu, which would mean rebuilding the tray on every job transition for a cosmetic gain.

### 9.6 `/api/status`

Additive and byte-identical when off, using `#[serde(flatten)]` over the existing internally-tagged `AppStatus` so the handler stays typed rather than degrading to `Json<Value>`:

```rust
#[derive(serde::Serialize)]
struct StatusResponse {
    #[serde(flatten)] app: AppStatus,
    #[serde(skip_serializing_if = "Option::is_none")] git: Option<GitStatusSummary>,
}

async fn api_status(State(s): State<HostState>) -> Json<StatusResponse> {
    let git = s.git.as_ref().filter(|g| g.status_api()).map(|g| g.status_summary());
    Json(StatusResponse { app: s.status.read().await.clone(), git })
}
```

`status_summary()` is built **purely from the registry, the job store, and the in-memory state file — zero libgit2 calls, zero filesystem access.** `assets/loading.html` polls this endpoint in a tight loop; a `Repository::open` + `statuses()` per poll per repo would be a genuine performance bug.

```json
{ "state": "ready", "target_url": "http://127.0.0.1:8080/",
  "git": { "registry_error": null,
           "repos": [ { "id": "notes", "path": "/…/repos/notes", "auto_sync_secs": 300,
                        "busy_job": null,
                        "last_sync": { "at_ms": 1785000004411, "ok": true, "op": "sync",
                                       "outcome": "merged", "job_id": "job_7c1e93aa_00002b",
                                       "code": null } } ] } }
```

Live tree state (`dirty`, `ahead`/`behind`) is deliberately absent — it needs libgit2. Callers use `GET /api/git/repos/:id/status`. The existing `status_endpoint_reports_states` test and `loading.html` are unaffected.

### 9.7 `restart_children` and `sync_settings`

`restart_children` never reaches `App` as a flag. `GitService::after_job` decides whether to send `GitRestartChildren`, requiring **all** of:

1. the request (or the repo's `restart_children_on_pull`) asked for it, **or** `sync_settings` produced a real change;
2. the operation actually moved HEAD or rewrote `settings.json` — **no restart on `up_to_date`**, which is what stops a 5-minute auto-sync from restarting Chrome 288 times a day;
3. the app status is `Ready` — otherwise the request is deferred into the job's `warnings` (a `sync_on_start` repo finishing before `wait_healthy` would otherwise fire a restart mid-startup; the existing `server_generation` guard makes that *correct*, but the user would see a double server start).

`sync_settings` (in `ops.rs`, inside the blocking job, reusing the existing code paths verbatim):

1. **Before staging:** copy `paths.settings_file` → `<tree>/<settings_path>`, materializing it from `settings::defaults(schema)` first if it does not exist yet, and record a hash of the bytes.
2. **After the merge**, if the tree copy's bytes changed during this job:
   - `serde_json::from_str` → `settings::validate_incoming(schema, &v)`;
   - **Ok** → `settings::save(&paths.settings_file, &values)`, `result.settings_synced = true`; and only if the *validated, normalized* values differ from those loaded at job start, set `settings_changed` (which forces a restart — `APP_SETTING_*` is injected at spawn).
   - **Err(e)** → do **not** touch the local `settings.json`. Set `result.settings_rejected = {"error": e}` as a **warning on an otherwise successful sync**, and write the host's own valid `settings.json` back into the tree so the next push heals the remote. A teammate's typo must not fail an entire git sync forever.
3. Because values round-trip through `settings::save`, key order and formatting are normalized, so two hosts syncing the same settings do not fight over whitespace. Write files and let git's filters normalize line endings — never diff bytes against a blob yourself.
4. `sync_settings = true` with no settings schema is a registry-validation error (§4.2), never a silent no-op.

**Only `settings::save` ever writes `settings.json`, and only `settings::validate_incoming` ever validates it.** Git does not get a second format.

---

## 10. Testing

Everything except the four `#[ignore]`d tests runs offline and deterministically on all three CI OSes. libgit2's local transport needs no network, no daemon, and no `https`/`ssh` feature.

### 10.1 Pure unit — no fs, no network (~40 tests)

- `git::valid_id` — the full table: `["..", "a/b", "a\\b", "../x", ".", "", "A", "con", "NUL", "x.", "x ", &"x".repeat(65)]` rejected; `["notes", "a.b-c_1", "com.example.notes"]` accepted.
- `git::contained_in` / purge containment — `repos/../..`, plus a `#[cfg(unix)]` symlink case.
- `error::classify` — one arm per row of §5.6 over constructed `git2::Error`s, **including `(GenericError, Callback)` → `timeout`/`canceled`** (the verified error a `transfer_progress` returning `false` actually produces — *not* `ErrorCode::User`).
- `error::scrub` — `https://u:p@h/x` → `https://h/x`; a stored token embedded in an arbitrary message.
- `secret` — `Debug` prints `***`; a doc-comment on `Secret` records that it must never gain a `Serialize` impl.
- `creds::resolve` — per-call wins whole; stored fallback; `None` fallback; `bound_host` mismatch → `auth_unbound`; the USERNAME probe does not consume the budget; attempt 3 → `auth_failed`; a grep-style assertion that `resolve` has no `Cred::default`/agent/helper arm.
- `config` — `[git]` defaults, every validation error string, absent section ⇒ `git.is_none()`, a typo'd key rejected by `deny_unknown_fields`.
- `tray::menu_model(.., show_sync=true)` order, mirroring the existing `full_model_order`.
- `supervisor::build_env` exports `APP_HOST_URL`/`APP_HOST_TOKEN`/`APP_GIT_ENABLED` (extends `build_env_layers_and_settings_file`).
- `utc_stamp` against known epochs including a leap day and the epoch itself.

### 10.2 `jobs.rs` — deterministic concurrency, **zero sleeps** (~12 tests)

`GitService` takes an injected `ops: Arc<dyn GitOps>`; the test impl blocks on a `std::sync::mpsc::Receiver` the test owns, and signals entry through a `Condvar`. This follows the template's existing precedent — `chrome::find_first_existing` takes an injected existence closure for the same reason.

- **`second_op_on_busy_repo_is_409_with_the_running_job_id`** — gate job A; `gate.wait_entered()` proves it is running; `admit` again → `Busy(a_id)`; assert `error.code == "repo_busy"`, `error.job.id == a_id`, `error.job.op == "sync"`; release; assert A reaches `succeeded`; assert the next `admit` is `Started`.
- **`replay_precedes_busy`** — `admit(r, Sync, Some("r1"))` → `Started`; `admit(r, Sync, Some("r2"))` → `Busy`; `admit(r, Sync, Some("r1"))` → **`Replay`, not `Busy`**.
- **`replay_of_a_failed_job_admits_a_fresh_one`** — the fix for the retry trap.
- `different_repos_run_in_parallel` — both gated jobs report entered before either releases.
- `lease_released_on_panic` — a panicking ops impl; the repo is admissible again and the job is `failed` with `internal`.
- `eviction` — insert 60 finished + 1 running; assert 50 retained, the running one survived, and **no job younger than 60 s was evicted** (injected clock).
- `ttl_expiry` with the injected clock; `request_id` reusable after eviction.
- State-machine invariants: `result.is_some() ⟺ Succeeded`; terminal states never mutate.
- `foreign_host_instance_job_id_is_not_found`.

### 10.3 `ops.rs` / `merge.rs` — real git2, tempdir, no network (~30 tests)

```rust
/// A bare "remote" plus two clones, so every push/pull/diverge scenario is
/// reproducible with no network, no credential, and no running server.
fn fixture() -> Fixture;                    // origin.git + repos_root + a GitService
fn worker(origin: &Path) -> Repository;     // a second clone standing in for "someone else"
```

The bare remote is `Repository::init_opts(&bare, RepositoryInitOptions::new().bare(true).initial_head("main"))` and remotes use the **plain absolute path** form (sidesteps Windows `file://` drive-letter questions entirely).

Covered: init (branch name honoured even when the developer's global `init.defaultBranch` is `master` — set via a scoped config in the fixture), clone, clone-into-populated-dir → `already_exists`, clone-into-non-repo → `not_a_repository`, `ensure_branch` after cloning a `master`-default remote, commit (dirty/clean/`allow_empty`/pathspec/caller author/default author), **first push sets upstream**, push fast-forward, push non-ff → `not_fast_forward`, `force:true` succeeds, pull (clean ff / up-to-date / dirty → `dirty_tree` / `commit_local:true`), sync on an absent tree → `cloned`, sync with `remote: null` → `committed`, unborn adopts remote, branch list/create/checkout/set-upstream, checkout on a dirty tree → `dirty_tree`, reset to head and to upstream, `clean_untracked` both ways, `branch_mismatch`, `detached_head`, status buckets + `MAX_DIRTY_FILES` truncation, purge and purge refusal on a symlinked tree.

**Merge policy — the tests that must not be hand-wavy:**

| test | setup | assertions |
|---|---|---|
| `prefers_local_whole_file_on_conflict` | both clones rewrite line 1 of `f.md`; the other pushes first | `outcome == "merged"`; `read(f.md)` equals the local content **byte-for-byte**; `!contains("<<<<<<<")`; `conflicts_resolved == ["f.md"]`; `head.parent_count() == 2` |
| `losing_remote_version_stays_recoverable` | same | `head.parent(1)?.tree()?.get_path("f.md")` blob == the remote content |
| `non_overlapping_edits_auto_merge` | A edits the top of `f.md`, B the bottom | the result contains **both** edits; `conflicts_resolved` is empty — *this is the test that catches a regression to `FileFavor::Ours`, which would splice hunks* |
| `local_delete_beats_remote_modify` | A deletes `f.md`, B modifies it | the file is absent; reachable via `^2`; no conflict remains |
| `remote_delete_loses_to_local_modify` | inverse | the file survives with local content |
| `add_add_with_different_content` | both create `new.txt` differently | local bytes survive; the index had a conflict that resolution cleared |
| `dir_file_collision_is_refused` | local file `x`; remote dir `x/inner.txt` | job fails with `merge_path_type_conflict`; **the local file still exists on disk** and HEAD did not move |
| `unrelated_histories_use_empty_base` | local `init`+commit vs a remote with its own root | `outcome == "merged_unrelated"`, both trees present |
| `fast_forward_does_not_create_a_merge_commit` | local behind only | `parent_count() == 1`, `outcome == "fast_forward"` |
| `up_to_date_is_a_noop` | | HEAD unchanged, no commit written |
| `untracked_file_survives_a_merge_sync` | scratch file in the tree | still present afterwards |
| `no_merge_state_is_ever_left` | after every case above | `!tree.join(".git/MERGE_HEAD").exists()`, `repo.state() == Clean`, `!head_detached()`, and no file in the worktree contains `<<<<<<<` |

`sync_settings` trio: valid pulled settings → written + `GitRestartChildren` observed on a test `mpsc::Receiver`; invalid pulled settings → local `settings.json` untouched, `settings_rejected` populated, job **succeeded**, and the tree copy healed back; unchanged settings → **no** restart event.

Conflict-notification trio, on the same `mpsc::Receiver`: a sync that resolved a conflict emits exactly one `GitConflictsResolved` carrying the merge commit and the overwritten paths; a sync with no conflicts emits **none**; and `App::git_notice_is_new` returns `true` once then `false` for the same merge commit id, with the bounded set evicting the oldest past 16 entries. The dialog call itself is not unit-tested (it is `rfd`, and modal) — the arm's guard chain is, by testing `git_notice_is_new` and the tray-mode branch directly.

**Test-only note, in a comment:** any test asserting on transfer progress or abort must set `RepoBuilder::clone_local(git2::build::CloneLocal::None)` — the default local-clone path bypasses the transport entirely and fires `transfer_progress` **zero** times, so such a test would otherwise assert on nothing. Also: the local transport does **not** run server hooks, so server-side rejection cannot be unit-tested this way (covered by an `#[ignore]`d test).

### 10.4 `registry.rs` / `state.rs` with `tempfile` (~15 tests)

Round-trip and defaults; `deny_unknown_fields`; **`redaction_never_emits_a_secret`** — `assert!(!serde_json::to_string(&view).unwrap().contains("ghp_SUPERSECRET"))` across all three credential kinds; corrupt file → quarantined to `repos.json.corrupt-*` **and the original bytes preserved in the quarantine file**; `version != 1` → quarantined; one bad entry → the others load, `rejected` populated, and **a subsequent save preserves the rejected raw entry verbatim**; `repos.json.tmp` cleaned at load; `#[cfg(unix)]` mode is `0o600` and a loose mode is tightened; a missing `private_key_path` is accepted; `git-state.json` corruption is non-fatal.

### 10.5 axum router (~18 tests)

`spawn_git_state(enabled: bool, ops: Arc<dyn GitOps>) -> (HostState, u16, TempDir)` mirrors the existing `spawn_state` helper exactly.

404 `git_disabled` on every path when `[git]` is absent; **401 on every path including GETs**, looping over the route table; 422 on `../evil`, `A`, a 200-char id; 404 on an unknown repo; `PUT` → 201 then 200 with `updated_at_ms` advanced; a blunt leak canary `assert!(!body.contains("github_pat"))` over the full response text; `POST /sync` → 202 + `Location`; replay → 200 + the same id; 409 body shape end-to-end (pre-poisoned via a gated job); `DELETE ?purge=true` removes the tree; `DELETE` on a busy repo → 409; `registry_writes = false` → 403 on PUT/DELETE but 200 on GET; `job_not_found` carries `host_instance`; **`/api/status` without `status_api` has no `git` key and still has `state`/`target_url`** (the backward-compat guard for `loading.html`), with it on the `git` key appears.

### 10.6 `#[ignore]`d integration (4 tests)

Following the existing `real_chrome_reports_version` convention:

```rust
#[test] #[ignore = "requires network; run with cargo test -- --ignored"]
fn vendored_tls_trusts_public_ca() { /* clone https://github.com/rust-lang/libc.git */ }

#[test] #[ignore = "requires GIT_TEST_HTTPS_URL and GIT_TEST_TOKEN"]
fn real_https_clone_and_push_and_server_rejection() { /* also exercises push_update_reference */ }

#[test] #[ignore = "requires GIT_TEST_SSH_URL and GIT_TEST_SSH_KEY"]
fn real_ssh_clone_pins_the_host_key() { }

#[test] #[ignore = "requires network; asserts a wrong token fails fast instead of looping 15×"]
fn wrong_token_fails_once() { /* assert elapsed < 30s && code == auth_failed */ }
```

The first is the canary for the vendored-TLS story and should be run manually on every distro the app ships to. The last is the only honest test of the retry cap against a real server.

CI stays `cargo test --locked` on ubuntu/macos/windows with no secrets and no network.

---

## 11. Security

README §5 already states the truth this builds on: **the `x-host-token` is CSRF protection against other web pages, not isolation from other local processes.** Any process running as the same user can `GET /loading`, read `{{TOKEN}}` out of the HTML, and drive every mutating endpoint. That does not change. What changes is the blast radius: before this feature the host held **no credentials**; now it may hold PATs, SSH passphrases, and inline private keys.

### What a loopback attacker gains, and the response

| new capability | severity | response |
|---|---|---|
| **Read back a stored token or passphrase over HTTP** | would be the worst outcome | **Eliminated, at compile time.** `Secret` has no `Serialize`; `RepoView` has no secret-shaped fields. There is no code path from a stored secret to a response body. |
| **Exfiltrate a credential by repointing a repo at an attacker host** | the worst *remaining* primitive | **Eliminated by `bound_host`.** A stored credential is only ever sent to the host it was stored against; a `PUT` that changes the host drops it with a warning; an operation needing an unbound credential fails `auth_unbound` rather than transmitting. |
| **Use a stored credential to push attacker content to the user's real remote** | medium, and genuinely new | Not preventable at this trust level — the credential is on disk and usable by `git` directly. Made **auditable**: every operation is logged to `git.log` with repo, op, trigger, and outcome. Mitigate operationally with a fine-grained PAT scoped to one repository. |
| **Write files anywhere via a crafted id** | — | **Eliminated.** The id charset cannot express `..`, `/`, or an absolute path; the tree is always `repos_dir.join(id)`. |
| **Delete files anywhere via `?purge=true`** | — | **Eliminated.** `canonicalize` on both ends, refuse if outside or equal to the root, refuse symlinks (§7.11). Refusal is 403 and logged. |
| **Execute code via a malicious repository** | — | **Structurally eliminated.** libgit2 is a library; no `git` subprocess is ever spawned, so hooks (`post-checkout`, `post-merge`), `core.sshCommand`, `GIT_SSH_COMMAND`, credential helpers, aliases, and external clean/smudge filters are all inert. This is a materially stronger position than shelling out to `git` and is the main reason `git2` is the right dependency. |
| **MITM an SSH sync** | new | **Addressed** by TOFU host-key pinning, default on. |
| **MITM an HTTPS sync / downgrade to plaintext** | | Full TLS verification (WinHTTP / SecureTransport / vendored OpenSSL); `certificate_check` passes TLS certs through rather than approving them; there is no insecure-TLS flag; `http://` is rejected unless `allow_http`. |
| **SSRF-ish outbound requests** | low | Limited to git transports, and responses are never returned to the caller. `registry_writes = false` limits remotes to those the author declared. |
| **Denial of service** | low | One job per repo caps work regardless of request volume. |
| **Enumerate remotes and absolute paths** | low | Token required on every route, GETs included — defence in depth, not a boundary. |

Two more properties stated explicitly: `repos.json` is `0600` on unix and tightened on load if found looser (on Windows it inherits the `%APPDATA%` ACL — documented, not enforced); and **`APP_HOST_TOKEN` in the child's environment adds no exposure** — a same-user process can read `/proc/<pid>/environ`, but that principal could already read the token from `/loading`. Chrome never receives it.

**Not solved:** the token is still same-user readable. Closing that requires OS-level peer authentication — a unix domain socket with `SO_PEERCRED`, a Windows named pipe with an ACL — replacing the TCP listener entirely. Out of scope (§13).

README §5 gains a paragraph making the delta explicit: *enabling `[git]` means the host stores credentials; the loopback token protects against other **web pages**, not other **processes** run by the same user. Prefer a fine-grained PAT scoped to the single repository the app needs — the host will only ever send a stored credential to the host it was stored against. If your repos are author-declared, set `registry_writes = false`.*

---

## 12. Build & packaging

### 12.1 Cargo

```toml
# git2 0.21 has `default = []` — a bare `git2 = "0.21"` is LOCAL-ONLY: clone/fetch/push
# over the network are silently unavailable. `ssh_key_from_memory` is NOT a feature in
# 0.21 (naming it is a hard error); Cred::ssh_key_from_memory is unconditional.
git2 = { version = "0.21", features = ["https", "ssh", "vendored-libgit2"] }
gethostname = "1"

# libssh2-sys declares a NON-OPTIONAL openssl-sys dependency on cfg(unix) — and
# cfg(unix) includes macOS. libgit2's own HTTPS backend is SecureTransport there, so
# git2's `vendored-openssl` feature (gated to cfg(all(unix, not(macos)))) does NOT
# cover it. Declaring openssl-sys directly here unifies the `vendored` feature across
# the whole graph on every unix target, so a clean machine with no system libssl
# builds. Windows uses WinHTTP + WinCNG and pulls in no OpenSSL at all.
[target.'cfg(unix)'.dependencies]
openssl-sys = { version = "0.9", features = ["vendored"] }
```

**[VERIFY] — resolution-half CLOSED (commit `fadba71`), build-half still open.** The dependency-graph claim is confirmed: with the deps as written above, `cargo tree -i openssl-src --target aarch64-apple-darwin` resolves `openssl-src v300.6.1+3.6.3 → openssl-sys v0.9.117`, reached by three edges — our direct `[target.'cfg(unix)']` declaration, `libgit2-sys`, and `libssh2-sys`. Note that `git2`'s *own* optional `openssl-sys` edge is **absent** on macOS (Linux shows four edges, macOS three), which is exactly why the direct declaration is load-bearing rather than redundant. `x86_64-pc-windows-msvc` resolves no OpenSSL at all (`nothing to print`).

What a resolution check **cannot** establish, and what still needs a real macOS runner: that the vendored OpenSSL actually *compiles and links* there with no Homebrew OpenSSL present. Keep this item open until the first green macOS CI leg.

`LIBGIT2_NO_VENDOR=1` (plus `LIBSSH2_SYS_USE_PKG_CONFIG=1`) remains available as a distro-packager escape hatch and is documented in README §7.

**No cargo feature gate for git.** Decision 2 chose a *runtime* gate and decision 15 asks for a plain `cargo build`; a compile-time axis doubles the CI matrix and creates "compiles for me" bugs. One binary, one configuration. README documents the three-step removal for authors who want the build time back: delete `src/git/`, the `mod git;` line, and the `git2`/`openssl-sys`/`gethostname` deps. (Alternative: `[features] default = ["git"]` — reconsider only if template users report the build time as intolerable.)

### 12.2 Per-OS reality

| | Linux | macOS | Windows MSVC |
|---|---|---|---|
| libgit2 HTTPS backend | `GIT_OPENSSL` | `GIT_SECURE_TRANSPORT` | `GIT_WINHTTP` |
| libssh2 crypto | `LIBSSH2_OPENSSL` | `LIBSSH2_OPENSSL` | `LIBSSH2_WINCNG` |
| `openssl-sys` in graph | yes | **yes (via libssh2)** | no |
| build prerequisites | C toolchain, **`perl`**, **`make`**, `pkg-config` | Xcode CLT, **`perl`**, **`make`** | MSVC toolchain only |

**CA certificates on Linux are handled for us**: `git2::init()` calls `openssl_probe::init_ssl_cert_env_vars()` whenever `https` is on. No extra dependency and no `set_ssl_cert_locations` call is needed; the `vendored_tls_trusts_public_ca` `#[ignore]`d test is the canary.

### 12.3 Cost

Measured on an 8-core box, rustc 1.95: clean build **+~80 s per OS** (Windows is the fast leg, having no OpenSSL to build — OpenSSL vendoring is ~70 % of the time); stripped release binary **343 KB → ~7.9 MB**. Static archives: `libcrypto.a` 15 MB, `libgit2.a` 4.2 MB, `libssl.a` 2.6 MB, `libssh2.a` 655 KB. `ldd` on the result shows only `libc`/`libgcc` — the `.deb`/`.dmg`/`.msi` stay genuinely self-contained.

CI: both workflows already use `Swatinem/rust-cache@v2`, which absorbs the cost after the first run; note in the README that `~/.cargo/registry` alone does not help — the cache must cover `target/*/build/{libgit2-sys,libssh2-sys,openssl-sys}-*`. Add `perl` and `pkg-config` to the Linux apt line. `macos-latest` and `windows-latest` need no new steps.

### 12.4 README

New §9 "Git service", opening with the ~20-line runnable Node quickstart (define → sync → poll → use `repo.path` with ordinary `fs` calls), then the endpoint table, the `[git]` key table in §3, the env contract in §4, the security delta in §5, `repos.json` / `git-state.json` / `repos/<id>/` rows in §6, and the build prerequisites in §7. §9 must say **in bold** that prefer-local loses remote edits on conflict and is the wrong policy for repos with concurrent human editors, and must document that `error_dialogs` covers both failures and conflict-resolved syncs.

---

## 13. Deliberately NOT included

| cut | why |
|---|---|
| **Job cancellation endpoint** | Only the network phase is interruptible; a cancel that silently no-ops during merge, checkout, and connect is worse than none. Replaced by `network_timeout_secs`, libgit2's global connect/idle timeouts, and a visible `stalled` flag. |
| **Force-with-lease** | Not supported by git2 0.21 — `PushOptions` has no expected-old-oid parameter. The fetch-compare-then-force approximation is a TOCTOU window, not a lease. |
| **Cargo feature gate for git** | §12.1. |
| **Rebase, cherry-pick, revert, stash, squash** | Decision 7 fixes the merge policy. Rebase would rewrite the history that policy explicitly preserves for recoverability. |
| **Submodules, LFS, sparse/shallow/partial clone** | Each is a large libgit2 surface with its own failure modes; a shallow repo that cannot fetch its own merge base fails in a way the API cannot explain. |
| **Tags, notes, multiple remotes per repo** | `AutotagOption::None`. The sync algorithm is defined against exactly one remote; a second remote is a second repo definition pointing at the same tree, which the design already permits. |
| **Diff / log / blame / file-content endpoints** | The API returns the absolute tree path; the child reads and writes it with its own language's I/O. Proxying files through HTTP would double every byte and create a second, worse filesystem API. *This is the largest surface-area saving in the design.* |
| **A conflict-reporting / review flow** | By policy conflicts are always resolved, never surfaced for a decision. The user is *told* (`result.merge.conflicts_resolved`, the merge commit message, `git.log`, and the `error_dialogs` notice in §9.3) and given the `git show …^2:<path>` recovery command — but a review flow implies a UI, a paused state machine, and a resume path. |
| **SSE / WebSocket / webhooks on jobs** | The child already polls `/api/status`; a 300 ms poll is fine for jobs measured in seconds, and EventSource cannot set the auth header. |
| **Job persistence across restarts** | `host_instance` + effect-idempotent ops solve the caller's actual problem with three fields instead of a database, and avoid a job-vs-reality reconciliation problem on every boot. |
| **`ssh_agent` credential kind, `Cred::default()`, credential helpers** | §8.3. |
| **Insecure-TLS escape hatch** | The one flag most likely to be turned on "temporarily" and never off. |
| **Credential encryption / OS keychain / `zeroize`** | `0600` matches `~/.git-credentials`. A keychain adds a dependency, a platform matrix, and a first-use prompt from a headless process. Rust `String`s are moved and reallocated freely, so zeroing one copy is false assurance for real cost. |
| **GPG/SSH commit signing** | Needs external key management, an agent, and a verification policy. |
| **Proxy configuration** | `FetchOptions::proxy_options` is one line; proxy auth, PAC files, and `NO_PROXY` are not. |
| **`unlock` endpoint for a stale `index.lock`** | Startup cleanup handles the real case; the error message names the file for the rest. |
| **Registry hot-reload / external-edit detection** | Read once at startup, written by the API. Documented. |
| **Retrofitting the JSON error envelope onto `/api/settings` and `/api/restart`** | Would break existing tests and any template already consuming bare-string bodies, for cosmetic uniformity. |
| **`.gitignore` management, `git gc`, per-file API, multi-tenant identity** | The caller owns the tree; there is one caller. |
| **Unix-socket transport with peer-credential auth** | The real fix for §11's "not solved", but it replaces the TCP listener entirely. |

Would add on request, cheapest first: `merge_policy = "prefer_remote" | "report"` (~60 LOC — the merge is already parameterized); `GET /repos/:id/log?limit=N` (~40 LOC, the most likely first integrator request); `POST /jobs/:id/cancel` (~30 LOC); `POST /repos/:id/unlock` (~15 LOC); `[git].proxy`; shallow clone; a `keyring`-backed credential store.

---

## 14. Risks

| # | risk | failure mode | mitigation in this design |
|---|---|---|---|
| 1 | **Prefer-local discards a collaborator's edit.** This is the *specified* policy: a user editing the same note on two machines loses one version. | Perceived data loss; a support burden the template author inherits. | Four redundant records: the merge commit message enumerates every overwritten path *and* prints the recovery command; `result.merge.conflicts_resolved` lists them; `git.log` records them; and — this is the decided override of the original recommendation — a **dialog names the files and the recovery command when `error_dialogs` is on** (§9.3), de-duplicated per merge commit and suppressed in tray mode. The remote version is permanently reachable as parent 2. README §9 says in bold that this policy is wrong for repos with concurrent human editors. Residual risk: an app that leaves `error_dialogs = false` (the default) still loses the edit silently as far as the end user is concerned — the server author is expected to surface `conflicts_resolved` in their own UI. |
| 2 | **Vendored OpenSSL is a build-time landmine on Linux and macOS.** `perl` + `make` are required, not just a C toolchain, contrary to decision 15's letter; a first build dies with an opaque `openssl-src` error. | A first-time template user's very first `cargo build` fails. | The explicit `[target.'cfg(unix)']` openssl-sys line (the common myth that macOS needs no OpenSSL is wrong — libssh2 does); README §7 prerequisite table; CI proves all three OSes; the removal recipe for authors who don't need git. |
| 3 | **`sync_settings` restarting Chrome under the user.** Every restart tears down and relaunches the window, discarding scroll position and in-page state. | Intermittent, unreproducible loss of in-page state — the worst kind of bug report. | Restart only on an actual validated-value change (hash compare, not mtime); auto triggers never set `restart_children`; suppressed while `in_tray_mode` or while status ≠ `Ready`; 2 s debounce. Residual risk is real and is a bold README warning. |
| 4 | **`add_all(["*"])` scooping the caller's junk.** The host is a generic executor; a `node_modules` in the tree gets committed. | A five-minute "sync", a remote nobody can clone, a first push that fails a size limit. | `IndexAddOption::DEFAULT` honours `.gitignore`, `FORCE` is never used, `dirty` is capped at 200 entries, and the README states that the caller owns `.gitignore`. Not fully solvable — it is the caller's tree by design. |
| 5 | **The subsystem is bigger than the host** (~2 850 LOC against ~2 200). | The template stops reading as "a small Rust host" and starts reading as "a git client that also opens Chrome". | Entirely under `src/git/`, entirely inert when `[git]` is absent, and removable in three edits. Honest, not eliminated. |
| 6 | **A hung remote pins a blocking thread.** Our watchdog lives in the transfer callbacks, which never fire before the transport has data. | With several auto-syncing repos the blocking pool degrades; repos stay 409-busy. | libgit2's process-global `set_server_connect_timeout_in_milliseconds(10s)` and `set_server_timeout_in_milliseconds(network_timeout_secs)` (§7.1) — these, not the callbacks, are what actually bound a black-holed connect. The lease is deliberately **not** released early: two concurrent libgit2 operations on one working tree would corrupt the index. Visible-but-stuck (`stalled: true`) beats silently-corrupt. |
| 7 | **`IndexEntry` reconstruction after `remove_path`.** `IndexEntry` is not `Clone`; the stage bits live in `flags`. | If stage-0 re-add does not supersede the removed stages, `write_tree_to` fails with `code=Unmerged` on *every* conflicting sync — a repo that worked for weeks starts failing permanently. | The entry is **moved** out of `IndexConflict.our` (never cloned) and `flags`/`flags_extended` are zeroed, per the verified recipe; a hard runtime check (`merge_unresolvable`) plus `debug_assert!` follow; the add/add and modify/delete tests exercise it directly. This is the single riskiest ~15 lines in the feature. |
| 8 | **Hand-edited `repos.json`.** Read once at startup; a `PUT` rewrites the whole file. | Silent config loss, blamed on the app. | The file is *never* written by a sync, a timer, or a job (all volatile state lives in `git-state.json`), rejected entries are preserved verbatim across rewrites, and `registry_writes = false` makes the file fully author-owned. Documented as not hot-reloaded. |
| 9 | **Windows path length, file locking, non-UTF-8 paths.** `<data-dir>/repos/<id>/` plus a deep repo path can cross `MAX_PATH`; antivirus makes `purge` fail; a latin-1 filename is not representable. | Cryptic `io_failed` errors on Windows only, invisible to Linux developers. | 64-char id cap; OS message passed through in `message`; `bytes_to_path` is cfg-gated and errors explicitly on Windows rather than silently mangling; CI runs the merge fixtures on `windows-latest`; README note. |
| 10 | **Jobs lost on host restart.** | A naive client's poll loop spins on 404 forever. | `host_instance` in every response and in the `job_not_found` envelope; the id itself carries the instance; a documented recovery recipe; `last_sync` in `git-state.json` is durable across both eviction and restart. |
| 11 | **`settings.json` write race** between a `sync_settings` pull and `POST /api/settings`. | One user-initiated settings change is silently lost. | Both go through `settings::save`, so the file never corrupts — only a lost update, in a sub-second window between two deliberate user actions. Fixing it properly would couple two subsystems for a rare race. Accepted and documented. |
| 12 | **`quit()` now waits.** | "Quit" appears to hang on a wedged network. | `kill_children()` runs **first**, so the window and server die immediately and the user sees the app close; hard bound `quit_sync_timeout_secs + 2`, validated ≤ 120, default 10, and default no repos opted in. |
| 13 | **Vendored OpenSSL and CVE latency.** | The shipped `.deb`/`.dmg` carries a statically linked OpenSSL that only a rebuild can patch. | Documented, with the `LIBGIT2_NO_VENDOR=1` + `LIBSSH2_SYS_USE_PKG_CONFIG=1` escape hatch for distro packagers. |
| 14 | **Noisy merge commits.** Every diverged sync writes one. | An active two-machine setup produces a merge commit per interval. | Accepted — decision 7 requires it, and it is what makes the losing remote version recoverable. |
| 15 | **`x-host-token` on git GETs diverges from `/api/status`.** | A template author following the existing pattern is briefly confused. | One README sentence. |

---

## 15. Implementation slices

Each slice is independently shippable, compiles clean under `cargo fmt` + `clippy -D warnings`, and lands with its own tests.

| # | slice | definition of done |
|---|---|---|
| **0** | **Split `main.rs` → `main.rs` + `host.rs`.** Pure move, no behaviour change. | `cargo test` passes unchanged; `git diff --stat` shows only moves; `main.rs` ≤ 310 LOC. |
| **1** | **Build spine.** Add `git2` + target-gated `openssl-sys` + `gethostname`; a `src/git/mod.rs` stub that only prints `git2::Version::get()` behind a `#[test]`. | `cargo build --locked` succeeds on ubuntu, macos, and windows CI; the version test asserts `vendored && https && ssh && threads`. |
| **2** | **Config + paths.** `GitSection`, `SshHostKeyPolicy`, `validate_branch_name`, the `validate()` arm, `RuntimePaths::{repos_dir, registry_file, git_state_file}`, the commented `[git]` block in `app.toml`. | Every validation rule has a test asserting the exact error string; an `app.toml` with no `[git]` still parses; `ensure()` is unchanged. |
| **3** | **`secret.rs` + `error.rs`.** `Secret`, `GitError`, codes, `http_status()`, `retryable()`, `classify()`, `scrub()`, `IntoResponse`. | The classification table test covers every row of §5.6 including `(GenericError, Callback)`; `scrub` tests pass; `Secret` has no `Serialize`. |
| **4** | **`registry.rs` + `state.rs`.** Load, tolerant parse, quarantine, atomic 0600 save, `RepoView`, id validation, `bound_host` population. | §10.4 passes, including the rejected-entries-preserved and `0600` tests; no git2 import in either file. |
| **5** | **`jobs.rs`.** `JobStore`, `admit`, `RepoLease`, progress, retention, `request_id` index, host-instance ids. | §10.2 passes with **zero `sleep` calls**; no git2 import. |
| **6** | **`ops.rs` core + `creds.rs`.** `init`, `clone`, `commit`, `status`, `branches`, `branch`, `reset`, `purge`; `resolve`, the callback builder, TOFU. Local-path remotes only. | The non-merge half of §10.3 passes, plus §10.1's credential table; `initial_head` pinning is proven by a test with a `master`-defaulting scoped git config. |
| **7** | **`merge.rs` + `sync`/`pull`/`push`.** The prefer-local merge, the dir/file guard, fetch/analysis ordering, push refspecs, `push_update_reference`, `set_upstream` after first push. | The full merge matrix in §10.3 passes, including `dir_file_collision_is_refused` and `non_overlapping_edits_auto_merge`; `no_merge_state_is_ever_left` holds after every case. |
| **8** | **`GitService` + `api.rs`.** Wiring, the axum sub-router, the auth layer, DTOs, error envelope, `git.log`, `git2::opts` timeouts, startup recovery. | §10.5 passes; `/api/git/*` 404s as `git_disabled` when `[git]` is absent; no route answers without the token. |
| **9** | **Host wiring.** `HostAccess` in `build_env`, the three `HostEvent` variants and their arms, tray `GitSync`, `StatusResponse` flatten, `sync_on_start`, `quit()` reordering + `run_quit_syncs`, auto-sync timers, dialog gating (failure **and** conflict-resolved, with `git_notice_is_new` de-duplication). | `build_env` and `menu_model` tests pass; `/api/status` is byte-identical with `status_api = false`; the conflict-notification trio passes; a manual smoke run shows a tray sync, a restart-on-settings-change, one conflict dialog for a two-clone overwrite (and **not** a second on retry), and a clean quit with an in-flight job. |
| **10** | **`sync_settings` helper.** Copy-before-stage, validate-after-pull, `settings::save`, tree healing, the change-gated restart. | The §10.3 `sync_settings` trio passes, including "invalid pulled settings leave the local file untouched and heal the tree copy". |
| **11** | **Docs + CI.** README §9 (Node quickstart first), §3/§4/§5/§6/§7 updates, `perl`+`pkg-config` on the Linux apt line, cache-path note, the four `#[ignore]`d integration tests. | `cargo test --locked` green on all three OSes; the four ignored tests pass when run manually with credentials; README has no TODOs. |