# Host-Side Git Service Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> **This plan is split across files.** This document is the index and the global constraints. Each task lives in its own file under `2026-08-03-git-service/`, because the full plan is ~20,000 lines and a single-file `Read` would silently truncate to the wrong task. **Every task implementer must read `2026-08-03-git-service/interface-contract.md` first** — it is authoritative over the spec for every name, signature, and stable string.

**Goal:** Give the Rust host a generic git executor — define, init, clone, pull, push, sync, inspect and delete repositories on behalf of the local `[server]` child, over the existing loopback HTTP API.

**Architecture:** A new `src/git/` subsystem behind a runtime `[git]` config gate. Repositories are declared in a writable `<data-dir>/repos.json` registry and checked out under `<data-dir>/repos/<id>/`. Mutating calls return `202 + job id` and run on `spawn_blocking` with one job per repo; callers poll `GET /api/git/jobs/:id`. The subsystem never touches `App`, `Children`, or the restart generation counters — its only channel into the tao event loop is `HostEvent`.

**Tech Stack:** Rust 2021, `git2` 0.21 (vendored libgit2 1.9.6 + libssh2 + OpenSSL), axum 0.8, tokio, serde/serde_json, `gethostname`, `tempfile` for tests.

**Spec:** `docs/superpowers/specs/2026-08-03-git-service-design.md` — 1,882 lines, approved. Read §0 (the 16 locked decisions) and §2 (architecture) before starting anything.

## Global Constraints

- Rust stable, `edition = "2021"`, `rust-version = "1.85"`. Package/binary name `hitch`.
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
| 11 | §14 risk 11's accepted settings write race is now a WHY comment at the `write_settings(&sc.settings_file, …)` call in `settings_apply_back`: last-writer-wins over one small document, never corruption (`settings::save` writes the whole map in one call, so no reader sees a half-written file), worst case one lost update that the next save or sync republishes. The comment also forbids the tempting "fix" — skipping the write when the file moved underneath would silently drop the *remote's* values instead, which is the strictly worse loss |
| 11 | *(found during execution)* Step 11's `origin_with_settings` fixture calls `testkit::clone_at(&o, "seed")`, but `origin_with_main` already creates and keeps a `seed` tree at that path; libgit2 refuses with `"'…/seed' exists and is not an empty directory"`. The fixture now reuses that tree |
| 11 | *(same)* Step 16's `settings_sync_events` fixture defines its repo with no `remote`, so `start_job(…, JobOp::Sync, …)` refuses admission with `remote_missing` before a job record exists and both restart tests panic in the fixture. It now points at a path inside the tempdir; `SettingsOps` never opens it. **Superseded by F5a below** — the admission guard was the defect, not the fixture. The fake remote is gone again and the fixture is back to `remote: null`, which is what task 11 wrote in the first place |
| 11 | *(same)* Step 11's `mirror_job` helper takes `host_settings: &PathBuf`, which fails `clippy::ptr_arg` under `-D warnings`. Changed to `&std::path::Path` |
| 11 | Steps 17/18 came out the way the plan predicted: both restart tests pass with **no** edit to `decide_restart`. Mutation-verified rather than assumed — deleting the `|| out.settings_changed` OR-term gives "expected exactly one restart request, got []", and pinning `reason` to `"requested"` gives "a restart caused by the settings mirror must say so" |
| 11 | *(same)* The plan's claim that task 8 added `use crate::git::merge::testkit;` to `ops.rs`'s test module is wrong — task 8 put the testkit to work in `merge.rs`, not `ops.rs`. The import is added here |
| 12 | Step 16's grep expectations were a range ("accept two or three hits") that could not fail. Replaced with exact measured counts and a one-line note per count saying which sections produce them: `perl make pkg-config` 2, `LIBGIT2_NO_VENDOR=1` 1, `APP_GIT_ENABLED` 2, `git-state.json` 3, `registry_writes` 3, `error_dialogs` 3 |
| 12 | §14 risk 3 is now a blockquote in §9.6: a settings change arriving from the remote relaunches Chrome and discards scroll position and in-page state, which is why the restart is gated on a validated *value* differing rather than on the file changing |
| 12 | §14 risk 11 is now a second blockquote there: the write-back races the settings page, last writer wins the whole file, it can never corrupt (the file is always written whole), worst case is one lost update — and the obvious "fix" would silently drop the *remote's* values instead |
| 12 | *(found during execution)* Steps 8 and 36 both `grep -n 'TODO\|TBD\|FIXME' README.md` and expect **no output**, but §7 has always carried "code signing and notarization … are a per-app TODO" — deliberate prose telling the template user what they still owe. Neither grep could ever pass. Both are now scoped to §9 onward |
| 12 | `registry_writes` appeared only twice (§3 table, §5 delta) against the plan's "at least three". §9.4 gained the paragraph that was actually missing: `PUT` answers 201 then 200 on replace, and `registry_writes = false` turns both `PUT` and `DELETE` into `403 registry_read_only` |
| 12 | Two of the four network tests were run here and pass: `vendored_tls_trusts_public_ca` (17.3 s, a real clone of rust-lang/libc — the vendored OpenSSL build does find this platform's CA store) and `wrong_token_fails_once` (0.4 s to `auth_failed`, so the `MAX_CRED_ATTEMPTS` cap is holding rather than replaying fifteen times). `real_https_clone_and_push_and_server_rejection` and `real_ssh_clone_pins_the_host_key` need a scratch repository and credentials and are left for the maintainer |

### Still to apply

Nothing. Every row above was applied and re-measured during execution.

One of the two contract-vs-task drifts flagged here is now closed: `Registry::notes()` is in
contract §5.4, because the post-execution audit gave it a guarantee (scrubbed at construction)
that has to be stated somewhere a caller will look. `StateStore::load_error() -> Option<&str>`
is still absent from §5.5 and still additive rather than contradictory.

## Post-execution audit fixes

A full audit of the shipped subsystem found **eight** defects, fixed across eight commits, plus a
test-suite race found and fixed in a ninth while verifying them. **None widened the wire
vocabulary**: `GitErrorCode`, `as_str`, `http_status`, `retryable` and `classify` are byte-identical
before and after — `git diff a7427cc..HEAD -- src/git/error.rs` adds two named constructors and one
test-only line and touches nothing else — so the error-code tables in README §9.5, spec §5.6 and
contract §3.4 gained **zero rows**. Two footnote-level edits were owed and are applied:
`repo_locked` moved from execution-only to also an admission code, and it gained a second producer.

**How the plan and task files were treated.** Every task step reproduces the code it landed, so the
steps behind these eight would have rebuilt the defects on a re-execution — one of them a
credential-theft primitive. The rule applied, uniformly: **where the fix replaced a block a step
writes, the step is corrected in place and carries an `> Amended by the post-execution audit`
blockquote** naming what it used to say and why the shipped shape differs; **where the fix added a
facility the task never had, it is appended as an explicitly labelled step at the end of that task**
(task 5's step 63, `hold_repo`). That is the convention this repo already used for the task-2 and
task-3 amendments in the spec and the contract: the code blocks are instructions and have to be safe
to follow, while this ledger is where the history lives. A step file that silently disagrees with
`src/` is the one outcome worse than either. Touched: tasks 3, 4, 5, 7, 8, 9 and 12, plus
`interface-contract.md` and the spec.

| fix | task | finding |
|---|---|---|
| **F1** | 5 | `drop_job` lacked the ownership test `RepoLease::drop` has, so retention evicting a superseded failed record deleted the **live successor's** `request_id` entry and turned the next matching retry into the 409 that §6.2's replay-before-busy rule promises is impossible. Guarded on `by_request.get(&key) == Some(id)`; the two guards now cross-reference each other as one rule about two indexes. Spec §6.5 rule 5 was stating the defect and is reworded |
| **F2** | 4 | `put`'s carry-forward predicate treated an absent `bound_host` as permission to bind to **any** host (`(None, _) => true`). A loopback PUT repointing a repo at an attacker's remote kept the stored PAT, and `normalise_def` then rebound it to the attacker's host — the theft with the rebinding done for free. Now the same equality `creds::resolve` transmits by, `None` included, with the `kind: "none"` short-circuit kept so a credential that does not exist is never "dropped" with a warning. The warning is rebuilt from two half-clauses so `(Some, None)` and `(None, Some)` both read as English |
| **F3** | 4 | `load` turned every non-`NotFound` read error into `registry_corrupt` with `quarantined_to: None` **while leaving the bytes at `path`**, and `writable()` consulted only the config key — so the first `PUT` renamed a temp file over a `repos.json` full of other definitions and stored PATs, with no `.corrupt-` copy. The read is split (`fs::read` + `from_utf8`: bytes we hold and cannot use are quarantined, bytes we never got are left alone), and the latch is derived from `error` by one predicate shared by `writable` and the new `ensure_writable`. `quarantine`'s failed-rename arm no longer claims "; quarantined". Accepted trade, documented in README §9.7: a child that re-declares its repos on boot now gets a hard 403 and zero repos until a human intervenes |
| **F4** | 4 | `Registry::load` built every note and `RegistryError` with `format!` over `repos.json` and handed them out unscrubbed into `git.log` and into `GET /api/git`; the `insecure_remote` refusal that warns about passwords in remote URLs was itself printing one, on every launch, forever. `Registry::assembled` — the one funnel all eleven return paths pass through — now scrubs `notes` and the error, `rejected[].id` included; `StateStore::load` got the same one-line treatment. Closed by the second round below: a secret typed into a wrongly-typed field used to come back inside serde's own echo, which `error::scrub_serde` now redacts |
| **F5a** | 9, 11 | Step 43's admission guard listed `JobOp::Sync` among the verbs a `remote: null` repo may not run, contradicting spec §5.6 (which lists only `clone`/`pull`/`push`) and README §9.4 (which promises such a repo's sync "inits and commits"). A local-only repo could therefore never sync at all — the task-11 fixture row above was a workaround for exactly this. The rule the guard now states: admission refuses a verb only when **every** mode of it is impossible, which is also why `reset` is not in the list |
| **F5b** | 8 | `ops::sync` refused an absent tree with `no_worktree` when there was no remote to clone from. It now `init`s and falls through — `init` and not `create_dir_all`, because only `init` pins `refs/heads/<def.branch>` against the developer's global `init.defaultBranch`. The spec's own ops test plan had listed "sync with `remote: null` → `committed`" and it was never written; it is written now |
| **F6** | 8 | All three merge checkouts forced, and two of the three moved a ref **first**. Measured against the vendored libgit2 1.9.6: `force` does not *delete* an untracked file, it **overwrites** the ones that collide with the target tree (`checkout.c`: `GIT_DELTA_ADDED` with a workdir entry → `FORCE ? UPDATE_BLOB : CONFLICT`), so the shipped fast-forward lost data on **success**, not only on failure — and safe mode alone still clobbers a gitignored path the remote tracks, which needs the separate `overwrite_ignored(false)`. Now one `checkout_or_refuse` — `safe()` **first**, `overwrite_ignored(false)`, `recreate_missing(true)`, `notify_on(CONFLICT)` — run **before** any ref moves, in all three arms. The builder-call order is load-bearing and untestable: `safe()`/`force()`/`dry_run()` are `checkout_opts &= !((1 << 4) - 1)` and `RECREATE_MISSING` is `1 << 2`, inside that mask, so reordering silently un-sets it and no assertion in the suite can see it. Guarded by a comment, and that is all there is |
| **F7** | 7, 8 | Nothing on the write path read `repo.state()`, so a tree a human left in `RepositoryState::Merge` was accepted by every mutating verb: `add_all` staged the `<<<<<<<` markers as stage 0, the commit took HEAD as its only parent so the merged branch vanished, `merge_prefer_local`'s `cleanup_state()` then deleted their `MERGE_HEAD`, and `push` published it. Measured, not reasoned — the pre-fix `sync` returned `Ok`/`"merged"`. `open_tree` now calls `require_clean_state`, shared with `fill_status` so the status read and the refusal are one message; `reset` opens through `open_tree_any_state` because `git_reset` ends in `git_repository_state_cleanup`. `push` is gated deliberately even though real git allows it |
| **F8** | 5, 9 | `JobStore::busy` drops the lock before returning, so `put_repo`/`delete_repo` only **sampled** it; an auto-sync tick admitted in the gap left libgit2 writing into the directory `remove_dir_all` was walking. `busy` now maps to a `BusyBy` (a job **or** the host), `hold_repo` takes the same entry `admit` takes under one acquisition, and the refused-purge early return no longer leaves a `last_sync` row and job records for an unregistered id. `repo_locked` rather than `repo_busy`, because there is no job to embed and `api.rs` documents a client as keying off `error.job`'s presence |
| **flake** | 7, 8 | *(found while verifying the eight, not by the audit)* `ops::tests::hostile_global_config` mutates libgit2 **process-global** state — `git2::opts::set_search_path` for four `ConfigLevel`s — and its SAFETY comment argued the `OnceLock::get_or_init` was sufficient synchronisation because "every test enters through `Fixture::*` before it touches git2". That premise was false: `merge::testkit` never called it, so merge tests could be **inside** libgit2 while an ops fixture flipped the search path. Measured before the fix: 40 consecutive full-suite runs, 1 failed, `testkit::clone_at` panicking with `"the repository is not empty"`. Mechanism, read out of vendored libgit2 1.9.6: `clone_into` (clone.c) refuses when `git_repository_is_empty` is false, and that function (repository.c) compares HEAD's symbolic target against `git_repository_initialbranch`, which reads `init.defaultBranch` back through the global search path — flip the path between a clone's `git_repository_init` and its emptiness check and the fresh repository stops looking empty. Reproduced deliberately with a throwaway probe (one thread flipping, one cloning): 172–188 of 400 clones failed with that exact error, against 0 of 400 with no flipper. The helper is hoisted into `merge::testkit` as the crate's **single gate onto libgit2 in tests**, called at the top of every entry point there that can reach it — plus `git::mod`'s `service_planted` and `git::error`'s refused-connect test, which reach libgit2 without naming it. `ops::tests::Fixture` calls testkit's rather than keeping a copy: a second `OnceLock` would be a second, unordered mutation of the same global. A race cannot be pinned by an assertion, so verification is empirical: 250 consecutive full-suite runs, 250 passed. At the pre-fix rate that outcome has probability ~0.002 |

### Second round — the audit's own fixes, audited

A six-lens adversarial review of the eight fixes above found **seventeen** further defects, fixed
across nine commits. Three were regressions introduced by the first round; the rest were older and
had simply never been looked at from this angle. One is the worst thing found in the subsystem so
far, and it was not a regression: the settings mirror wrote through a symlink.

**The wire vocabulary is still untouched.** `GitErrorCode`, `as_str`, `http_status`, `retryable`
and `classify` gained nothing — the two new refusals reuse `path_refused` and `insecure_remote`,
which already existed. One `warnings[].code` value was added, `settings_symlink_replaced`, and
contract §3.11 records it: the alternative was repairing a hostile tree entry silently, which
would leave an operator with a repo that syncs fine and a remote still aimed at their filesystem.

Same treatment of the task files as the first round, and for the same reason: tasks 3, 4, 5, 7, 8,
9 and 11 each reproduce code these fixes replaced, and one of those blocks — task 11's settings
mirror — is an arbitrary-file-write primitive if followed as written. Every one carries an
`> Amended by the post-execution audit (second round)` blockquote.

| # | where | finding |
|---|---|---|
| **1** | `ops.rs`, task 11 | **[high]** The settings mirror wrote through a symlink. A remote chooses the *file type* at `settings_path`: track it as a mode-120000 blob and libgit2 materialises a real symlink (`blob_content_to_link` → `p_symlink`, checkout.c:1596), after which `settings::save`'s `std::fs::write` follows it and every sync overwrites whatever the remote named — a dotfile, a shell rc, or `<data-dir>/repos.json` with every stored PAT in it. Silently and forever: the index and HEAD still hold mode 120000, so nothing is ever staged and the job answers `no_changes`. `validate_settings_path` constrains the *string* and says nothing about the *file*. Proven end-to-end against a bare origin with a 120000 entry pointing outside the repos root. `settings_tree_target` is now the only way to that path, on the read side as well as the write; the leaf is unlinked and replaced (the tree copy is the disposable one, and the next commit heals every other clone), a linked *directory* component is refused 403 (`settings::save` `create_dir_all`s the parent, so a nested `settings_path` would otherwise escape with the leaf never being a link). Component by component rather than by canonicalising, so a link that stays *inside* the tree — where `.git/config` lives — is refused too. A SAFE checkout does not close this: `checkout_action_with_wd` sends a blob→link TYPECHANGE to CONFLICT only when the workdir copy is *modified*, which the copy-in plus the commit after it guarantee it is not, so the link can also land mid-job |
| **2** | `merge.rs`, task 8 | **[medium, regression]** F6's checkout-before-ref-move opened a window it did not close. Measured with a control arm: a stale `.git/refs/heads/main.lock` — what a killed `git` leaves, and which nothing swept — makes the checkout succeed and the ref write fail, leaving the working tree at the remote's content with HEAD on the old tip. `sync` stages and commits *before* it fetches, so the next run committed that as a local commit, diverged, and pushed a two-parent merge to the shared remote. As shipped: `origin tip moved=true parents=2`. With the pre-F6 order: `parents=1`. `lock_branch` takes the ref's lock through a `git2::Transaction` **before** the checkout, so a lock that cannot be taken is found while the job is still a no-op; `git_transaction_commit` afterwards only renames a lock file it already holds. All three arms follow it, `set_head` moves ahead of the checkout in both `analyse` arms, and `merge_prefer_local` writes its commit object with `None` before the checkout — an object write is a pure add, so an unreferenced commit is garbage and one `git gc` |
| **3** | `registry.rs`, `error.rs`, tasks 3–4 | **[medium]** `validate_remote` refused `user:pw@` and accepted `https://ghp_x@host/…`, which is how a PAT is actually written. `ops::init` and `reconcile_remote` then wrote it into a 0664 `.git/config` under a 0775 `repos/`, beside a 0600 `repos.json`. `error::strip_userinfo` had always scrubbed a bare `user@` and said why in its own doc comment — the scrubber knew and the validator did not. Any non-empty userinfo on http(s) is refused now; ssh and git are exempt, because their username is a transport identity. `insecure_remote` also scrubs at construction, since a `put` that refuses returns straight to the caller and the 422 body was quoting the token back |
| **4** | `jobs.rs`, task 5 | **[medium]** `a_maintenance_hold_and_a_job_exclude_each_other` stated the replay-past-a-hold rule in a comment and pinned nothing — a mutation putting the `BusyBy::Maintenance` check above `admit`'s replay lookup left the whole suite green, while turning a client's legitimate retry during a concurrent PUT into the 409 §6.2 promises is impossible. The case it *did* test, an unknown `request_id`, is behaviourally identical to the `None` case two lines above. Both are covered now, with distinct ids |
| **5** | `merge.rs`, task 8 | **[medium]** The "why not `dry_run()`" justification was wrong on both halves, and wrong in the direction that makes a footgun look safe. git2 0.21 spells `dry_run()` as `GIT_CHECKOUT_NONE` (1 << 30), not libgit2's `GIT_CHECKOUT_DRY_RUN` (1 << 24). NONE does force every delta action to NONE, so nothing is counted or notified — but it does **not** short-circuit the apply passes (checkout.c:2640 tests DRY_RUN only), and `checkout_action_wd_only` is not one of the four functions with the NONE early exit the comment claimed was universal. Measured: `dry_run()` plus `remove_untracked(true)` returned Ok, fired the callback, and deleted the file |
| **6** | `ops.rs`, task 7 | **[low, regression]** F7's gate ran *after* `open_tree_any_state` had reconciled the remote, so a verb refused with `dirty_tree` had already repointed `.git/config` on the one tree the refusal promises not to touch. Measured. The open and the reconciliation are split so the gate can sit between them; `reset` still reconciles, because it is not refusing and a reset to `upstream` has to resolve against the url the definition names |
| **7** | `ops.rs`, task 7 | **[low, regression]** F7's gate was bypassed by `clone`'s idempotent branch and `init`'s `Exists` branch, so one repo answered `200 up_to_date` / `200 initialized` to those two and `409 dirty_tree` to everything else — and `init` was not read-only about it, since that branch adds a missing remote. `open_tree`'s own doc comment claimed the gate was exhaustive. Both branches hold it directly now |
| **8** | `mod.rs`, task 9 | **[low]** Nothing swept stale `.git/refs/**/*.lock`, which is the trigger for #2 and left a repository unsyncable until a human went looking for a file they had no reason to know about. `recover_index_locks` keeps its name and now sweeps `index.lock`, `HEAD.lock`, `packed-refs.lock` and every `*.lock` under `.git/refs` at any depth — any depth, because a branch name may contain `/` |
| **9** | `mod.rs`, task 7 | **[low]** `network_timeouts_are_installed_from_the_configured_value` wrote libgit2's two process-global timeout options and read them straight back, under a SAFETY note claiming "nothing else in this test binary writes it". Seven tests reach `GitService::start`, which installs whatever *their* config says. Same shape as the `init.defaultBranch` race the first round fixed, same answer: one gate, every writer through it |
| **10** | `registry.rs`, task 4 | **[low, regression]** F3's read-failure arm promises the file "was left exactly as it is" and `tighten_mode` ran two lines above the read, so it did not — and for the case that arm exists to serve, a directory planted where the file belongs, chmod 0600 strips the search bit. The implementer already knew: the shipped test pre-chmodded the directory to 0700 with a comment explaining why. The workaround lived in the test rather than the code. It runs after the successful read now, and the mode is asserted |
| **11** | `registry.rs`, `state.rs`, task 3–4 | **[low]** serde echoes the value it choked on, so a hand-written `{"credential": "<token>"}` came back as `invalid type: string "<token>", …` and went into a 0644 `git.log` on every launch. `error::scrub_serde` redacts double-quoted runs longer than 20 characters and is used only where the input is a serde message; `scrub` is unchanged, because a libgit2 message's quoted strings are paths and URLs a reader needs |
| **12** | `error.rs` | **[low]** `RepoLocked` sat below the `// Execution` divider, which stopped being true when F8 gave it a synchronous admission producer. Left in place with a note, since the execution producer is the older and commoner of the two |
| **13** | `merge.rs`, task 8 | **[low]** "`recreate_missing(true)` fires only for a path with no working-directory entry" is not what checkout.c does: `checkout_action_with_wd_dir_empty` routes an empty directory through the same reader and removes it. The load-bearing half — nothing with content can be lost — survives |
| **14** | `merge.rs`, task 8 | **[low]** "with no baseline supplied libgit2 defaults it to the HEAD tree" omitted the `data->index->on_disk` guard (checkout.c:2483), and the omitted branch behaves oppositely: no index means an empty baseline and a force-added RECREATE_MISSING, so the same call writes the whole tree |
| **15** | `merge.rs`, task 8 | **[nit]** `testkit`'s "the three that do neither" undercounted: `write_file` and `read_file` also neither name `git2` nor call the gate. They are pure `std::fs` and never reach libgit2, which is the only reason they may skip it |
| **16** | `merge.rs` | **[nit]** `testkit_pins_a_default_branch_that_disagrees_with_main`'s parenthetical described a falsification that does not falsify: libgit2's built-in default is already `master`, so a gate that redirected the search paths and wrote nothing still passed. It asserts the config value the gate is responsible for now — the one thing there that cannot be true by accident. Verified: emptying the gate's config body turns it red |
| **17** | `mod.rs` | **[nit]** Two `println!("PROBE …")` lines left over from a debugging session |

**Every fix above is pinned by a test that fails without it.** Each was checked by mutation:
reverting the fix in place and confirming the new test goes red, rather than trusting that a green
suite means anything.

### Third round — what CI found that no audit could

The two audit rounds above were run entirely on Linux, and so was every gate before the merge.
The definition of done below asks for green on **ubuntu, macos and windows**, and until
`feat/git-service` landed on `main` that line had never actually been tested: CI fires on push
to `main` and on pull requests, the branch was pushed without a PR, and the last `main` build
predated the whole subsystem. It took two CI rounds to clear: the first died at `clippy` before
a single test ran, and only the second exposed **ten failing tests from three further root
causes** — none of the four findable by reading the code on one platform.

**The wire vocabulary is still untouched.** Nothing here adds, renames or re-codes anything;
one refusal that was already `invalid_request` is now documented as also firing on Windows.

| # | where | tests | finding |
|---|---|---|---|
| **1** | `registry.rs`, task 4 | all three legs, no tests ran | **[blocking]** `clippy::question_mark` in Rust 1.97 sees through `match host.find(']') { Some(j) => …, None => return None }`. CI pins `dtolnay/rust-toolchain@stable`, which floats; the local gate was 1.95 and genuinely clean, so the merge turned every matrix leg red on a lint that did not exist when the code was written. The rewrite is what the lint asks for and what the same function already does one arm above. The `remote_host` table gains a row for the unterminated bracket, because the arm was otherwise vacuous and `Some("[::1")` is a host a stored credential could bind to that is not the host we would connect to |
| **2** | `mod.rs`, tasks 5–9 | **8** | **[blocking]** The job-test fixture `const REMOTE: &str = "/nonexistent/origin.git"` is not an absolute path on Windows — `Path::is_absolute` wants a drive or UNC prefix there — so `start_job`'s pre-admission `validate_remote` refused it and eight job tests failed at their scaffolding, before touching anything they were written to test. `cfg!(windows)`-selected now. Nothing asserts on the literal or builds a path from it, so the swap is inert: every use is `Some(REMOTE)` into `repo_def` |
| **3** | `registry.rs`, task 4 | **1** | **[blocking]** `validate_remote_accepts_the_documented_forms` asserted `/srv/repos/x.git` is accepted, which is true only off Windows. Same cause as #2, different site, and the one that says what the product should do: the behaviour is right and was right, because a drive-relative remote stored now and resolved later, against whatever drive happens to be current, is exactly the ambiguity worth refusing. It was the fixtures and the *documentation* that were POSIX-only — README §9 and spec §4.2 both said "an absolute local path" flat. The test now asserts the foreign form's refusal as well, so the dependence is pinned in both directions |
| **4** | `supervisor.rs` | **1** | **[blocking, pre-existing]** `wait_healthy_succeeds_once_server_responds` has failed on `windows-latest` since before this subsystem existed — it was the sole failure on `main@9e03fad2` in July and nobody was watching. Its stand-in server writes the response and drops the socket without ever reading the request, and closing a socket whose receive buffer still holds unread bytes is an *abortive* close on Windows: the teardown outruns the response, hyper fails the exchange in `SendRequest`, and every poll fails until the 5s budget expires. Systematic, not flaky. The server drains the request before answering; the test asserts the drained request, so the fix is pinned everywhere rather than believed about one platform. See the measurement below — the first shipped explanation of *why* was half wrong |

Finding 4 also exposed why the diagnosis needed a whole extra CI round: `wait_healthy` discarded
the transport error, so its timeout could say only *that* the server never came up. It now
carries the last error or status into the message, which is the difference between a CI log that
names a connection reset and one that names nothing.

**Verified the way the other rounds were**, with one exception that has since been closed twice
over. Findings 1–3 are pinned by tests checked by mutation — reverting the fix in place and
confirming the new assertion goes red. Finding 4's *cause* could not be reproduced on any machine
available here: what mutation pinned was only that the drain happens at all, and the diagnosis
itself rested on Winsock's documented `closesocket` behaviour, the systematic failure shape, and
the control arm that `internal_server`'s loopback tests — a real axum server, which reads its
requests — passed on the same run. CI was named the adjudicator and agreed: `main@8af95cb` is
green on all three legs, and `windows-latest` ran **378 tests, 373 passed, 0 failed, 5 ignored**,
the previous run's 363 plus exactly the ten that were failing.

#### Finding 4, measured

A green suite proves the *fix* and says nothing about the *explanation*, and the fix changed two
things at once. So the 2×2 was run on the runner itself, on a throwaway branch opened as a PR —
`pull_request` is the only trigger that reaches Windows without touching `main` — with the server
answering five requests per configuration:

| drain | shutdown | | `os=windows` | `os=linux` |
|---|---|---|---|---|
| ✗ | ✗ | the old server | **0/5**, os error 10053 | 5/5 |
| ✓ | ✗ | | **5/5** | 5/5 |
| ✗ | ✓ | | **0/5**, os error 10053 | 5/5 |
| ✓ | ✓ | as first shipped | **5/5** | 5/5 |

Two things this settles, and the shipped explanation got one of them wrong:

- **The drain is the entire fix.** A graceful `shutdown()` buys nothing — it is 0/5 without the
  drain and 5/5 is reached without it. The abort comes from the unread bytes, not from skipping
  the FIN. The `shutdown()` call and its "FIN, not RST" comment have been removed, because a line
  that reads as protective and measurably is not is exactly what would tempt the next reader to
  keep it and drop the read loop — the one combination that fails.
- **The errno is 10053, `WSAECONNABORTED`**, reported through hyper as
  `client error (SendRequest) <- connection error <- An established connection was aborted by the
  software in your host machine`. Not the 10054 `WSAECONNRESET` a peer's RST raises. The abortive
  close is real; "the stack sends RST instead of FIN and the reset outruns the response" named the
  wrong half of the teardown and the wrong error.

Linux cannot discriminate any of it — all four configurations pass here, which is why the bug
survived two audit rounds and a 60-run stability sweep.

Both halves of #2 and #3's premise are **measured on the runner rather than taken from the
docs**, and by the same run that failed. `Path::is_absolute` is false for a POSIX root there —
that is what `rejected "/srv/repos/x.git"` says. And it is true for `C:\…`: `resolve_cwd` asks
the very same predicate, `resolve_cwd_rules` asserts `resolve_cwd(Some(r"C:\abs"), base) ==
r"C:\abs"`, and that test passed on the run in question. Had the prefix not counted as absolute
the call would have answered `base.join(r"C:\abs")` and the assertion would have failed with it.

> The commit message on `eab7921` says "nine" job tests where the count is eight — the ninth
> git-suite failure is finding 3, which shares #2's cause but not its site. The table above is
> the count that was checked against the run's own failure list.

### Known gaps left open by this audit

- **F7's gate is TOCTOU.** A human who runs `git merge` in a tree *while a job is already running*
  still reaches `merge_prefer_local`'s `cleanup_state()`. The window shrinks from "any time at all"
  to "during a running job"; closing it would need a re-check inside `stage_and_commit` that nothing
  can reach, and unreachable code is banned here.
- **A mid-rebase repo is gated but may not be repairable through the API.** During a rebase HEAD is
  detached, so `reset` hits `require_branch` first and answers `detached_head`. F7's message says
  "Finish it by hand in the working tree, **or** discard it with POST …/reset" precisely so it is
  not a lie in that case, but the escape hatch is genuinely narrower for rebase than for merge.
- **A refused merge leaves dangling objects**: a tree from `idx.write_tree_to(repo)` (F6) and,
  since the second round, the merge commit itself, which `merge_prefer_local` now writes with
  `None` before the checkout. Both unreferenced and gc-able, no correctness impact. Writing the
  commit early is what removed the last fallible step from between the checkout and the ref
  move, and an unreachable object is a much cheaper thing to leave behind than that window was.
- **`lock_branch` narrows the post-checkout window; it does not eliminate it.** Once the ref lock
  is held, all that remains is `git_transaction_commit`'s rename, which can still fail on ENOSPC
  or EIO. Compensating afterwards was considered and rejected: the inverse checkout is itself a
  checkout that can refuse, so it would trade a rare window for a second thing that can fail —
  and in the unborn arm it would delete a pre-existing local file that happened to match the
  remote. The residue is a rename failure on a filesystem that is already failing.

## Definition of done for the whole feature

- `cargo test --locked` green on ubuntu, macos and windows CI.
- `cargo fmt --check` and `cargo clippy -- -D warnings` clean.
- `/api/git/*` returns 404 `git_disabled` when `[git]` is absent from `app.toml`, and no
  `repos.json`, `git-state.json` or `repos/` directory is created.
- `/api/status` is byte-identical to today's output when `status_api = false`.
- The four `#[ignore]`d network integration tests pass when run manually with real credentials.
- README §9 exists, opens with the runnable Node quickstart, and has no TODOs.
