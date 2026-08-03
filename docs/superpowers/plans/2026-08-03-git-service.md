# Host-Side Git Service Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> **This plan is split across files.** This document is the index and the global constraints. Each task lives in its own file under `2026-08-03-git-service/`, because the full plan is ~20,000 lines and a single-file `Read` would silently truncate to the wrong task. **Every task implementer must read `2026-08-03-git-service/interface-contract.md` first** — it is authoritative over the spec for every name, signature, and stable string.

**Goal:** Give the Rust host a generic git executor — define, init, clone, pull, push, sync, inspect and delete repositories on behalf of the local `[server]` child, over the existing loopback HTTP API.

**Architecture:** A new `src/git/` subsystem behind a runtime `[git]` config gate. Repositories are declared in a writable `<data-dir>/repos.json` registry and checked out under `<data-dir>/repos/<id>/`. Mutating calls return `202 + job id` and run on `spawn_blocking` with one job per repo; callers poll `GET /api/git/jobs/:id`. The subsystem never touches `App`, `Children`, or the restart generation counters — its only channel into the tao event loop is `HostEvent`.

**Tech Stack:** Rust 2021, `git2` 0.21 (vendored libgit2 1.9.6 + libssh2 + OpenSSL), axum 0.8, tokio, serde/serde_json, `gethostname`, `tempfile` for tests.

**Spec:** `docs/superpowers/specs/2026-08-03-git-service-design.md` — 1,882 lines, approved. Read §0 (the 16 locked decisions) and §2 (architecture) before starting anything.

## Global Constraints

- Rust stable, `edition = "2021"`, `rust-version = "1.85"`. Package/binary name `chrome-host-app`.
- `git2 = { version = "0.21", features = ["https", "ssh", "vendored-libgit2"] }`. **git2 0.21 has `default = []`** — omitting those features yields a silently local-only build with no network transports. `[target.'cfg(unix)'.dependencies] openssl-sys = { version = "0.9", features = ["vendored"] }` is required because `libssh2-sys` declares a **non-optional** `openssl-sys` dependency on all of `cfg(unix)`, macOS included.
- **No cargo feature gate for git.** One binary, one configuration. The gate is runtime: `[git]` absent from `app.toml` ⇒ `/api/git/*` returns 404 `git_disabled`, no registry file is created, and no libgit2 work happens.
- Every `/api/git/*` route requires the `x-host-token` header — **including GETs**. This is a deliberate divergence from the unauthenticated `/api/status`.
- `repos.json` is written `0600` where the OS supports it. Secrets are redacted as `"***"` in every GET. `Secret` has `Deserialize` but **deliberately no `Serialize`** — adding one is a security regression.
- Merge policy is **whole-file last-writer-wins preferring local**. Conflict markers must never reach the working tree, and `RepositoryState` must be `Clean` with no `MERGE_HEAD` after every operation.
- Repo ids match `[a-z0-9_-]{1,64}` and double as the directory name; Windows reserved names are rejected.
- House style, non-negotiable: unit tests in a `#[cfg(test)] mod tests` at the bottom of the file they test; `tempfile` for filesystem tests; comments explain **why** (invariants, race reasoning), never what; `cargo fmt` and `cargo clippy -- -D warnings` clean; no `unwrap()` in non-test code except where a panic is correct and is commented as such.
- `ops.rs`, `merge.rs`, `creds.rs`, `jobs.rs`, `registry.rs`, `state.rs`, `error.rs`, `secret.rs`, `util.rs` never name `axum`, `tokio`, `App`, `Children`, or `HostEvent`. `jobs.rs`, `registry.rs` and `state.rs` additionally never name `git2`.
- Every task ends with `cargo test`, `cargo fmt --check`, and `cargo clippy -- -D warnings` all passing.

## Pre-flight baseline

Recorded on a clean tree at commit `7d3222a`, before any task runs:

```
test result: ok. 32 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out
```

Task 0 is a pure move and must reproduce this line **exactly**.

## Verified against the real toolchain

These were executed, not assumed, before the plan was written (probe crate outside the repo):

- Vendored build links on a clean machine: **libgit2 1.9.6, `vendored=true https=true ssh=true threads=true`**, ~101 s clean build on 8 cores.
- `git2::Version` has **no** `libgit2_features()` method — use the separate `vendored()` / `https()` / `ssh()` / `threads()` accessors.
- Spec §7.6's `merge_prefer_local` sketch was transcribed near-verbatim and run against real repositories: whole-file prefer-local, the `FileFavor::Normal` auto-merge canary, the dir/file collision refusal, unrelated histories via an empty tree, `initial_head` pinning, hard-reset-leaves-untracked, and `set_upstream` after a manual push **all behave as the spec claims**.
- `push` of a non-fast-forward returns `code=NotFastForward class=Reference` and **`push_update_reference` never fires** — confirming both rejection paths are genuinely required.
- A `transfer_progress` callback returning `false` surfaces as `code=GenericError class=Callback`, message `indexer progress callback returned -1` — **not** `ErrorCode::User`. This is the exact §5.6 `classify()` arm.

## Tasks

Execute in order. Each is independently shippable and leaves the tree green.

| # | task | file |
|---|---|---|
| — | **Interface contract — read first** | [`interface-contract.md`](2026-08-03-git-service/interface-contract.md) |
| 0 | Split main.rs into main.rs + host.rs | [`task-00-split-main-rs.md`](2026-08-03-git-service/task-00-split-main-rs.md) — 10 steps |
| 1 | Build spine — vendored git2 | [`task-01-build-spine.md`](2026-08-03-git-service/task-01-build-spine.md) — 9 steps |
| 2 | Config section and runtime paths | [`task-02-config-and-paths.md`](2026-08-03-git-service/task-02-config-and-paths.md) — 26 steps |
| 3 | Secret and error types | [`task-03-secret-and-error.md`](2026-08-03-git-service/task-03-secret-and-error.md) — 37 steps |
| 4 | Registry and state files | [`task-04-registry-and-state.md`](2026-08-03-git-service/task-04-registry-and-state.md) — 62 steps |
| 5 | Job store | [`task-05-job-store.md`](2026-08-03-git-service/task-05-job-store.md) — 62 steps |
| 6 | Credential resolution and callbacks | [`task-06-credentials.md`](2026-08-03-git-service/task-06-credentials.md) — 61 steps |
| 7 | Core git operations | [`task-07-core-git-operations.md`](2026-08-03-git-service/task-07-core-git-operations.md) — 61 steps |
| 8 | Prefer-local merge, sync, pull, push | [`task-08-merge-sync-pull-push.md`](2026-08-03-git-service/task-08-merge-sync-pull-push.md) — 59 steps |
| 9 | GitService and the HTTP API | [`task-09-gitservice-and-api.md`](2026-08-03-git-service/task-09-gitservice-and-api.md) — 103 steps |
| 10 | Host wiring | [`task-10-host-wiring.md`](2026-08-03-git-service/task-10-host-wiring.md) — 39 steps |
| 11 | Settings sync helper | [`task-11-settings-sync.md`](2026-08-03-git-service/task-11-settings-sync.md) — 21 steps |
| 12 | Documentation and CI | [`task-12-docs-and-ci.md`](2026-08-03-git-service/task-12-docs-and-ci.md) — 36 steps |

## Outstanding audit fixes

Two independent auditors reviewed this plan. **All three blocking defects are fixed** (they were
concentrated in task 7: the `OpCtx`/`OpScratch` field mismatch, five append-vs-replace `E0428`
collisions, and the untrimmed task-6 dispatch test). The contract's staleness is fixed too.

The fixes below are **non-blocking** — the plan compiles and executes as written without them —
but each is a real finding and should be applied before or during the task it names. They are
listed here rather than dropped because the fix pass was cut short by an account spend limit.

| task | finding | fix |
|---|---|---|
| 2 | Step 26's expected test count is wrong (says 42, should be 43); intermediate counts also drift | Recount against the steps; the task adds ten tests, `runtime_paths_layout` is extended not added |
| 3 | `GitErrorCode::SettingsInvalid` / `settings_invalid` can never be emitted — invalid pulled settings are deliberately a warning on a *successful* job (§9.7) | Delete the variant and its wire string (contract already updated) |
| 3 | `GitError::auth_missing`, `auth_unbound`, `canceled`, `timeout` have zero call sites | Delete all four (contract already updated) |
| 4 | `#![allow(dead_code)]` in `registry.rs`/`state.rs` is redundant — task 3's attribute in `mod.rs` already propagates to child modules | Delete both, and the claim that task 9 removes them |
| 5 | `JobStore::lookup` never evicts, so `/jobs/{id}` can return a record `/jobs` reports as gone (§6.5) | Add `inner.evict(now)` to `lookup`, with a failing test using the injected clock (keep zero `sleep`s) |
| 5 | Test names diverge from the ones spec §10.2 names | Add a mapping table; do not rename the tests |
| 6 | `push_transfer_progress` is never registered, so a push reports no progress at all (§6.4) | Register it; note in a comment that it returns `()` and cannot cancel |
| 6 | `percent` is computed from `received_objects`; spec §6.4 specifies `indexed_objects` | Use `indexed_objects` — indexing lags receipt for the whole tail of a fetch |
| 8 | §14 risk 7's `debug_assert!` half is missing | Add `debug_assert!(!idx.has_conflicts(), ...)` above the hard `merge_unresolvable` check |
| 8 | Step 3's note suspends the `clippy -D warnings` gate for twenty steps across five commits | Add only `GitError`/`GitErrorCode` up front and the other four where first used |
| 9 | `GitService` stores its own `repos_dir`, the second source of truth task 4 explicitly forbids | Call `self.registry.repos_dir()` and drop the field (contract already updated) |
| 9 | `status_timeout` / HTTP 504 has no test at any level | Add a bounded-read test and a router test asserting 504 + `error.code == "status_timeout"` |
| 9 | The `403 path_refused` purge refusal is tested only at the `ops` level, never over HTTP | Add a `#[cfg(unix)]` router test symlinking `repos/notes` outside the root |
| 9 | Step 80 says "five pre-existing tests"; `internal_server.rs` has six | Change to six |
| 9, 10 | Task 2's five `#[allow(dead_code)]` attributes have no removal step; `#[allow]` does not error when it stops firing, so they rot silently | Task 9 removes them from `GitSection` + the three `RuntimePaths` fields; task 10 from `git_enabled()` |
| 10 | `git_log_path()` hard-codes `"git.log"`, re-opening the drift `GIT_LOG_FILE` exists to prevent | Use `crate::git::GIT_LOG_FILE` |
| 10 | Step 32 is dead conditional text patching `after_job` with locals that exist in no task | Delete it; make Step 31 an unconditional regression test |
| 10 | `quit()`'s ordering (the §14 risk 12 mitigation) has no machine check | Lift out `fn quit_sync_timeout(cfg: &AppConfig) -> Option<Duration>` and unit-test absent-`[git]` / 0 / 10 |
| 10 | Only the `status_api = false` half of the `/api/status` guard is tested | Add the `status_api = true` case asserting the `git` key appears |
| 10 | Definition of done says eight new tests; the steps sum to nine | Recount |
| 11 | Step 18 is dead conditional text whose "match the shape rather than the text" instruction invites a divergent second implementation | Delete it; state the invariant and keep the two characterization tests |
| 11 | §14 risk 11's accepted settings write race is undocumented | Add a WHY comment at the write-back call |
| 12 | Step 16's expected grep count is a range ("accept two or three hits") and cannot fail usefully | Pin it to an exact count or grep two specific anchors |
| 12 | §14 risk 3's consequence is never stated in the README | Bold warning: a validated settings change restarts the server child and relaunches Chrome, discarding scroll position and in-page state |
| 12 | §14 risk 11's race is never stated in the README | One sentence: never corrupts, worst case one lost update |

Two contract-vs-task drifts were also flagged and left alone as additive rather than contradictory:
task 9 consumes `Registry::notes() -> &[String]` and `StateStore::load_error() -> Option<&str>`,
neither of which appears in contract §5.4/§5.5.

## Definition of done for the whole feature

- `cargo test --locked` green on ubuntu, macos and windows CI.
- `cargo fmt --check` and `cargo clippy -- -D warnings` clean.
- `/api/git/*` returns 404 `git_disabled` when `[git]` is absent from `app.toml`, and no
  `repos.json`, `git-state.json` or `repos/` directory is created.
- `/api/status` is byte-identical to today's output when `status_api = false`.
- The four `#[ignore]`d network integration tests pass when run manually with real credentials.
- README §9 exists, opens with the runnable Node quickstart, and has no TODOs.
