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

### Already applied

| task | finding |
|---|---|
| 2 | Step 26's expected test count corrected 42 → 43. The auditor's second claim — that the intermediate `config::` counts drift — is **wrong**: 13/15/17/18/19 were re-derived against a measured baseline of nine `config::tests` and every one is right. Verified green end-to-end |
| 2 | *(found by the post-execution review, verified against real libgit2)* `validate_branch_name` checked `.lock` against the **whole name**, so `.`, `.x`, `a.`, `x.lock.`, `a/.b`, `x/.`, `a.lock/b` and `a/b.lock/c` were all admitted — every one refused by `git2::Reference::is_valid_name`. Replaced with per-component dot rules; contract §5.1 and spec both amended, since the gap was theirs. Cross-checked by a new `src/git/mod.rs` test asserting the one-directional invariant against libgit2 itself |
| 2 | *(same review)* `[git].author_name = " "` and an embedded NUL passed validation and then failed `git2::Signature::now` at runtime — on **every** commit, sync and merge — despite the error string promising "git signatures cannot represent them". `author_email` accepted a bare `@`, yielding the ident `App <@>`. Both tightened; one new stable string added for the name, `author_email` keeps its existing string with a stronger condition. `""` remains the fall-back-to-`[app].name` sentinel |
| 2 | *(same review)* The five `#[allow(dead_code)]` attributes cannot become `#[expect]` — the tests read every guarded item, so `dead_code` never fires under `cfg(test)` and the expectation is reported unfulfilled. Tried it; five warnings. The `#[allow]`s stay, each now carrying a comment saying why and which task deletes it. `git_enabled`'s comment named two call sites that never call it (`GitService::new` destructures `cfg.git`; `build_env` takes a pre-computed bool) — corrected to `host::App::start_children` |
| 2 | *(found during execution, not by the auditors)* Step 21's `shipped_git_block_is_commented_out_but_valid` was one-directional — every shipped value equals its serde default, so a `[git]` block that **lost** keys still deserialized into an indistinguishable `GitSection` and the test passed. Step 21 now pins `out.lines().count()` to 12; mutation-checked by deleting `allow_http` (`left: 11 / right: 12`). Step 23 gained an explicit instruction to leave the terminating blank line |
| 6 | `push_transfer_progress` is now registered in `creds::callbacks`, with `OpCtx::push_transfer` behind it. `transfer_progress` fires only on **fetch**, so without it a large first push looked hung for its whole duration. The comment records both differences libgit2's signature forces: it returns `()` so it **cannot cancel** (`sideband_progress` is the only abort hook during a push), and there is no separate indexed count because the server does the indexing |
| 8 | §14 risk 7's `debug_assert!` is now above the hard `merge_unresolvable` check, with a comment saying what it means: the loop resolves every conflict `conflicts()` reported, so a conflicted index at that point is a libgit2 behaviour change, not a merge outcome |
| 8 | Step 3's twenty-step clippy suspension is moot — the final state of `merge.rs` was written in one pass, so every import is used on arrival. `GitErrorCode` turned out to be needed only by the tests and moved into `mod tests` |
| 8 | *(found during execution)* **Both of step 56's "regression guards" were vacuous, and step 57's prescribed falsifications are both no-ops.** `untracked_file_survives_a_merge_sync` gitignores its scratch file — it has to, or `sync`'s `add -A` would commit it — and an ignored file is invisible to `remove_untracked`, so the test guards `remove_ignored` and the suggested falsification changes nothing. Renamed to `an_ignored_file_survives_a_merge_sync`, and a new `an_untracked_file_survives_a_merge_pull` reaches the same checkout through `pull` (which never stages) with a genuinely untracked file. Step 57's other falsification, `repo.merge(&[], None, None)`, returns `Err` and writes no `MERGE_HEAD` — probed and confirmed; falsify by writing the file directly instead. All three guards now verified to fail under the right mutation |
| 8 | *(same)* Two borrow-lifetime errors in the task's test code: `testkit::fetch_main` and the `remote_side` helper both return a value borrowed from a `Repository` dropped at the end of the block |
| 8 | *(same)* `git2::Oid::zero()` is deprecated in 0.21 (`Oid::ZERO_SHA1`), and the conflict-resolution `match c.our { Some(..) => .., None => {} }` trips `clippy::single_match`. Both fail `-D warnings` |
| 8 | *(same)* Task 7's `run_dispatches_every_verb_this_task_owns` asserted `run(Sync)` returns `internal` — true only while `sync` was a stub. Retargeted to prove the arm reaches the real verb |
| 7 | *(found during execution — a real production bug, caught by the task's own hostile-config fixture)* `require_branch` and `ensure_branch` used `Repository::is_empty()` to mean "HEAD is unborn". libgit2 defines empty as *"HEAD is symbolic, its target equals `init.defaultBranch`, and the repo holds no reference"* (`git_repository_is_empty`, repository.c), so a repo pinned to `[git].default_branch` on a machine whose git default differs reports **false** while holding no commits — and the very first commit into a fresh repo then failed with `unborn_branch`. Replaced with `head_is_unborn`, which asks `repo.head()` for `ErrorCode::UnbornBranch`. The plan's `require_branch_allows_an_unborn_head` could not catch it: it builds its repo with a plain `Repository::init`, which honours the machine default, so HEAD and the default agree by accident. Added `the_first_commit_into_a_freshly_initialised_repo_succeeds`, which does catch it (mutation-verified) |
| 7 | *(same)* Three more git2 0.21 API mismatches in the task's code: `expect_err` on `Result<Repository, _>` (no `Debug` on `Repository`), and `Signature::name()` / `Commit::message()` return `Result<&str, Error>`, not `Option<&str>` |
| 7 | *(same)* `status_walks_absent_uninitialized_unborn_clean_and_dirty` removed its stray file *after* `commit_file`, which stages with `add_all(["*"])` — so the stray was committed and its removal left a tracked deletion, making the tree dirty where the test asserts clean. The removal moves before the commit |
| 7 | *(same)* Task 6's test `Fixture` (ctx + lease) collides with task 7's tempdir `Fixture`. Task 8 is written against task 7's, so task 6's collapses into `Op`, which already has that exact shape |
| 6 | *(found during execution)* Two of the task's code blocks do not compile against git2 0.21: `HostKeyChecker::check` takes `&git2::Cert<'_>`, but `Cert` lives at `git2::cert::Cert` — the root re-export is `CertificateCheckStatus`, not `Cert`; and two host-key tests call `.expect_err(...)` on a `Result<CertificateCheckStatus, _>`, which cannot compile because `CertificateCheckStatus` derives **nothing**, `Debug` included. Fixed with a `refuse` helper matching the `refusal` helper the task already uses for `git2::Cred`, which has the same problem |
| 5 | `JobStore::lookup` now calls `inner.evict(now)` before it answers, so `GET /jobs/{id}` can no longer resurrect a record `GET /jobs` reports as gone. The test deliberately calls `lookup` **before** any `list` — checking `list` first would evict the record itself and make the test pass with or without the fix |
| 5 | Test-name mapping table against spec §10.2 added to the task; the tests are not renamed, because the names say what they assert |
| 3 | *(post-execution review, confirmed twice against real libgit2)* A refused or unreachable TCP connect carries class `Os`, not `Net`, so it classified as `io_failed` — 500, **non**-retryable — when it is the archetypal transient failure. A DNS miss carries class `Net` and *was* retryable, so "I typo'd the hostname" retried and "the host is down" did not. `classify` gained one guarded arm; spec §5.6's class partition and the contract's `IoFailed` row were both wrong and are amended. Grepping libgit2 for `"failed to connect` returns exactly three literals, so the prefix guard is precise rather than heuristic, and a hermetic loopback test pins the libgit2 behaviour it rests on |
| 3 | Step 9's expected result was PASS but the table test failed on arrival: deleting `SettingsInvalid` in the audit pass removed the variant and its row but left both `43` count literals. Corrected to a named `CODE_COUNT = 42` carrying the reason, in the code and in all four places in the task file |
| 3 | `GitErrorCode::SettingsInvalid` / `settings_invalid` deleted — invalid pulled settings are deliberately a warning on a *successful* job (§9.7), so nothing could ever emit the code |
| 3 | `GitError::auth_missing`, `auth_unbound`, `canceled`, `timeout` deleted — zero call sites; the four *codes* remain and are reached via `record_cred_failure` and `error::classify` |
| 6 | `percent` now computed from `indexed_objects`, per §6.4 — indexing lags receipt, so a received-based percent sits at 100% while indexing is still running |
| 9 | Step 80's count corrected: six pre-existing `internal_server` tests, seven total (verified against the source) |
| 10 | `git_log_path()` uses `crate::git::GIT_LOG_FILE` instead of a second `"git.log"` literal |
| 10 | Step 32 was dead conditional text patching `after_job` with four locals that exist in no task; it is now a review check against task 9's real `was_ok != Some(false)` gate |
| 11 | Step 18 had the same defect and the same fix — now a review check against task 9's real `decide_restart` |
| 4 | Verified at task 9's audit: `registry.rs` and `state.rs` carry no `#![allow(dead_code)]` — they were landed without one, so there was nothing to delete. `src/git/mod.rs:10`'s file-level attribute is the only one, and it is mutation-verified load-bearing |
| 9 | Verified at task 9's audit: `GitService` has no `repos_dir` field — every reader goes through `self.registry.repos_dir()` (12 call sites). The one derived copy, `repos_root_canon`, is the *canonicalised* root the containment guard compares against and is computed once in `build()`, so it is not a second source of truth for the location |
| 9 | `status_timeout` / 504 now has both halves: `git::tests::read_status_gives_up_at_the_ceiling_and_answers_status_timeout` at the service level and `internal_server::tests::a_wedged_status_read_answers_504_over_http` through the real axum stack. The router test also pins a fact the envelope makes easy to get wrong — `retryable` is the **job**-error shape only and is absent from the HTTP envelope, where the status code carries that meaning |
| 9 | The `403 path_refused` purge refusal now has a `#[cfg(unix)]` router test that symlinks `repos/notes` at a directory outside the root and asserts the target survives *and* the definition survives. It also pins that `error.path` names the **resolved** target (`…/elsewhere`), not the link — the plan had not said which, and the resolved form is the one a human has to go and look at |
| 9 | *(found during execution)* **Step 102's audit greps count their own documentation.** Every invariant is stated in a doc comment inside the file it constrains — `api.rs:3` says "No `git2` type crosses this file", `ops.rs:10` and `jobs.rs:18` name `tokio` in order to forbid it, `creds.rs:436` explains the `.expose()` count — so items 1, 4 and 5 could never report the value they claimed, and item 5 could never print `4`. All five commands now filter whole-line comments; item 1 gained `-H` (single-file `grep -n` emits no path, so the filter missed it), item 3 excludes the one sanctioned `HostEvent::GitRestartChildren`, item 5 excludes `secret.rs`'s own accessor test. Re-measured: items 1/3/4 clean, item 5 prints `4`. The invariants themselves all held — only the audit was unable to say so |
| 9 | *(same)* Task 2's `#[allow(dead_code)]` on `GitSection` and the three `RuntimePaths` fields are deleted, and their comments with them; `cargo clippy --all-targets -- -D warnings` is silent without them, which is the proof the readers landed |
| 10 | `quit_sync_timeout(cfg) -> Option<Duration>` is lifted out of `quit()` and unit-tested at absent-`[git]` / 0 / 10, plus a range assertion on the shipped default so a future edit to `default_quit_sync_timeout_secs` that made quit feel hung would fail the suite |
| 10 | `/api/status` now tests the `status_api = true` half. Worth noting for the README: `status_api` defaults to **false**, not true — the absent-case test alone would have passed against a handler that never filled the block |
| 10 | Definition of done recounted: **eleven** new tests, not eight. The original undercounted its own steps by one and predated the two the audit rows above added |
| 10 | *(smoke, run under Xvfb on `:77` so nothing touched the user's desktop)* All four of step 39's checks verified against a real bare remote: clone → `cloned`; a genuine both-sides edit → one merge commit, the local copy kept, `conflicts_resolved=["shared.txt"]` and the `git show <oid>^2:<path>` hint, with `GitConflictsResolved` reaching the tao loop; a second sync → `no_changes`; the remote moved away and synced **twice** → two `err` lines in `git.log` but exactly **one** `GitFailed` line in `host.log`, which is the ok→fail gate holding end to end; and closing the Chrome window (`on_close = "quit"`) → `quit` → `children: killed` → the quit sync committing and pushing `onquit.txt` **after** the children were gone. Also re-ran with `[git]` absent: `/api/status` byte-identical, `/api/git` → 404 `git_disabled`, and the data dir holding only `app.lock`, `chrome-profile` and `logs/{host,chrome}.log`. Not covered: the *visual* appearance of the two rfd modals — the run set `error_dialogs = false` so nothing blocked, and the events that drive them were asserted instead |

### Still to apply

| task | finding | fix |
|---|---|---|
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
