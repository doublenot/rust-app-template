### Task 12: Documentation and CI

This is the last slice. Tasks 0–11 have landed the whole git subsystem; nothing here
changes behaviour. It delivers three things: the README a template user actually reads,
the CI prerequisites the vendored OpenSSL build needs, and the four `#[ignore]`d network
tests that cover what an offline CI cannot.

**Files:**
- Modify: `README.md` — §2 build prerequisites (`README.md:31-33`), a new `[git]` table in
  §3 (inserted at `README.md:114`, just before `## 4`), §4 env contract (`README.md:125-129`
  plus a new subsection before `## 5` at `README.md:147`), §5 security delta (appended
  before `## 6` at `README.md:170`), §6 data-location rows (`README.md:186-189`), §7 build
  prerequisites (`README.md:208-218`), and a new §9 appended after `README.md:252`.
  *(Line numbers are pre-edit anchors; they shift as you go. Anchor on the heading text.)*
- Modify: `.github/workflows/ci.yml:16-24` — `perl` + `pkg-config` on the apt line, cache comment.
- Modify: `.github/workflows/release.yml:24-30` — the same two edits.
- Modify: `src/git/mod.rs` — append four `#[ignore]`d tests and their harness to the existing
  `#[cfg(test)] mod tests` block (the one task 1 created for the `git2::Version` assertion).

> **Ownership note for the consistency audit:** the interface contract's module table lists
> `src/git/mod.rs` as owned by task 1 with "later editors 9, 10". This task adds a *fourth*
> editor, test-only: the four network tests are subsystem-level (they exercise the
> `GitService` → `jobs` → `ops` → `creds` path end to end), not unit tests of any single
> file, and `src/git/mod.rs` is the subsystem root that already owns the build-configuration
> test. No non-test line in that file is touched.

**Interfaces:**
- Consumes (all already defined by tasks 2–9, nothing new is introduced):
  - `crate::config::AppConfig::from_str(&str) -> Result<AppConfig, String>`
  - `crate::config::RuntimePaths::under(&Path, &str) -> RuntimePaths`, `.ensure() -> io::Result<()>`,
    field `git_state_file: PathBuf`
  - `crate::internal_server::{AppStatus, HostEvent}`
  - `crate::git::GitService::new(&AppConfig, &RuntimePaths, tokio::runtime::Handle,
    tokio::sync::mpsc::UnboundedSender<HostEvent>, Arc<tokio::sync::RwLock<AppStatus>>)
    -> Option<Arc<GitService>>`
  - `GitService::{start, start_job, put_repo, tree_path, host_instance}`
  - `crate::git::StartOutcome::{Started, Replay, Busy}`
  - `crate::git::jobs::{JobOp, JobState, JobView}`, `JobSlot::{is_terminal, view}`, field `id`
  - `crate::git::ops::OpRequest::manual()`
  - `crate::git::registry::{RepoDef, CredentialSpec}`
  - `crate::git::secret::Secret::new(String)`
  - `crate::git::error::GitErrorCode::as_str(self) -> &'static str`
  - `crate::git::state::StateStore::{load, fingerprint, record_fingerprint}`
- Produces: nothing consumed by a later task (this is the last task). The only names it
  introduces are private to `src/git/mod.rs`'s test module: `Harness`, `TEST_TOML`,
  `harness`, `def`, `run_job`, `code_of`.
- Stable strings this task documents, which must match the contract character for character:
  env vars `APP_HOST_URL` / `APP_HOST_TOKEN` / `APP_GIT_ENABLED`; test-only env vars
  `GIT_TEST_HTTPS_URL` / `GIT_TEST_TOKEN` / `GIT_TEST_SSH_URL` / `GIT_TEST_SSH_KEY`;
  packager escape hatches `LIBGIT2_NO_VENDOR=1` / `LIBSSH2_SYS_USE_PKG_CONFIG=1`.

---

## Part A — README §9 "Git service"

- [ ] **Step 1: Append §9's opening — the runnable quickstart and the merge-policy warning**

`README.md` currently ends at line 252, at the end of `## 8. Releasing`. Append the
following to the end of the file. The quickstart comes **before** any endpoint table on
purpose: a template user must be able to see the shape of the thing in thirty seconds.

````markdown

## 9. Git service

Enabled by adding a `[git]` section to `app.toml` (§3). It gives the `[server]`
child an HTTP API for defining git repositories, syncing them, and then reading
and writing their working trees as ordinary directories. The host links
libgit2 — it never shells out to a `git` binary.

### 9.1 Quickstart

This runs as-is inside a Node `[server]` child. The host injects
`APP_HOST_URL` and `APP_HOST_TOKEN` into its environment (§4).

```js
import fs from "node:fs/promises";
import path from "node:path";

const base = `${process.env.APP_HOST_URL}/api/git`;
const headers = { "x-host-token": process.env.APP_HOST_TOKEN,
                  "content-type": "application/json" };
const call = async (p, init) => {
  const r = await fetch(base + p, { ...init, headers });
  const b = await r.json();
  if (!r.ok) throw new Error(`${p} -> ${r.status} ${b.error.code}: ${b.error.message}`);
  return b;
};

// 1. Define the repo. PUT is create-or-replace, so this is safe on every boot.
await call("/repos/notes", { method: "PUT", body: JSON.stringify({
  remote: "https://github.com/acme/notes.git",
  credential: { kind: "token", token: process.env.NOTES_PAT },
}) });

// 2. Sync it: clone if absent, else stage + commit + fetch + merge + push. 202.
let { job } = await call("/repos/notes/sync", { method: "POST",
  body: JSON.stringify({ request_id: "boot-1" }) });

// 3. Poll the job until it stops running.
while (job.state === "running") {
  await new Promise(r => setTimeout(r, 300));
  ({ job } = await call(`/jobs/${job.id}`));
}
if (job.state === "failed") throw new Error(`${job.error.code}: ${job.error.message}`);

// 4. The tree is just a directory. Use ordinary fs calls — there is no file API.
const { repo } = await call("/repos/notes");
await fs.writeFile(path.join(repo.path, "today.md"), "# hello\n");
```

Running step 2 again after step 4 commits `today.md` and pushes it. That is the
whole model: the host moves bytes between a remote and a directory, and your
code owns the directory.

### 9.2 The merge policy — read this before you enable anything

When a sync finds that the remote and the local tree have both changed the same
file, it resolves the conflict **by keeping the local file, whole**, and records
a merge commit whose second parent is the remote version.

> **Prefer-local means a sync can silently discard an edit made on another
> machine. This is the wrong policy for any repository with concurrent human
> editors.** It is the right policy for a repository that one app instance
> owns — notes, exports, generated state, a settings file — which is what this
> service is for.

Nothing is destroyed: the overwritten version stays permanently reachable as
parent 2 of the merge commit, and the sync reports which files it overwrote in
four places — `result.merge.conflicts_resolved` on the job, the merge commit
message, `<data-dir>/logs/git.log`, and (when `error_dialogs = true`) a native
dialog naming the files and printing the recovery command:

```bash
git show <merge_commit>^2:<path>
```

Non-overlapping edits to the same file merge normally — the whole-file rule
only applies to a hunk git could not reconcile on its own. Deletes follow the
same rule: a local delete beats a remote modify, and a local modify beats a
remote delete. A local file colliding with a remote *directory* of the same
name is refused outright (`merge_path_type_conflict`) rather than guessed at;
the tree is left untouched.
````

- [ ] **Step 2: Verify the opening landed intact**

Run:

```bash
grep -n '^## 9\. Git service' README.md
grep -n 'wrong policy for any repository with concurrent human editors' README.md
grep -c 'APP_HOST_TOKEN' README.md
```

Expected:

```
253:## 9. Git service
...:> **Prefer-local means a sync can silently discard an edit made on another
2
```

(The exact line numbers depend on trailing whitespace; what matters is that all
three greps produce a hit, that the warning phrase appears inside a `>` blockquote
line that also contains `**`, and that `APP_HOST_TOKEN` now appears twice — once
in the JS block and once in the prose above it.)

- [ ] **Step 3: Add §9.3, the endpoint table**

Append to the end of `README.md`:

````markdown

### 9.3 Endpoints

Base path `/api/git`. **Every** route requires the `x-host-token` header,
including the GETs — which diverges from `GET /api/status`, deliberately left
unauthenticated so `assets/loading.html` can poll it. Timestamps are epoch
milliseconds and carry an `_ms` suffix.

| method | path | body | success |
|---|---|---|---|
| GET | `/api/git` | — | `200` service info: `host_instance`, paths, defaults, feature flags, `registry_error` |
| GET | `/api/git/repos` | — | `200 {repos: [...]}` |
| PUT | `/api/git/repos/{id}` | repo definition (§9.4) | `201` created / `200` replaced, `{repo, warnings}` |
| GET | `/api/git/repos/{id}` | — | `200 {repo, status}` |
| DELETE | `/api/git/repos/{id}?purge=true` | — | `200 {deleted, purged, path}`; `purge=true` also deletes the working tree |
| GET | `/api/git/repos/{id}/status` | — | `200 {repo, status}` — branch, HEAD, upstream, ahead/behind, dirty file lists |
| GET | `/api/git/repos/{id}/branches` | — | `200 {current, detached, local, remote, upstream}` |
| POST | `/api/git/repos/{id}/init` | `{request_id?}` | `202 {job}` |
| POST | `/api/git/repos/{id}/clone` | `{request_id?, credential?}` | `202 {job}` |
| POST | `/api/git/repos/{id}/pull` | `{request_id?, credential?, commit_local?, restart_children?}` | `202 {job}` |
| POST | `/api/git/repos/{id}/push` | `{request_id?, credential?, force?}` | `202 {job}` |
| POST | `/api/git/repos/{id}/sync` | `{request_id?, credential?, message?, author?, allow_empty?, push?, force?, restart_children?}` | `202 {job}` |
| POST | `/api/git/repos/{id}/commit` | `{request_id?, message?, author?, paths?, allow_empty?}` | `202 {job}` |
| POST | `/api/git/repos/{id}/branch` | `{request_id?, name, create?, from?, checkout?, upstream?}` | `202 {job}` |
| POST | `/api/git/repos/{id}/reset` | `{request_id?, to?, clean_untracked?, confirm}` | `202 {job}` |
| GET | `/api/git/jobs?repo_id=&state=&limit=` | — | `200 {jobs: [...]}`, newest first |
| GET | `/api/git/jobs/{job_id}` | — | `200 {job}` |

Notes on the shape:

- **`sync` is the verb you want**, and the only one most callers need. Tree
  absent → clone. No `remote` → stage and commit only. Otherwise: stage
  everything not ignored, commit if dirty, fetch, merge (§9.2), push. Send
  `push: false` to keep it local.
- **The three GET reads are synchronous, not jobs**, and they deliberately do
  *not* wait for a job running on the same repo — observing a repo while it
  syncs is the main reason you would call them. They are bounded at 2 seconds
  and answer `504 status_timeout` past that. A read taken mid-checkout is a
  snapshot; the job's `result` is authoritative.
- **`PUT` is whole-object create-or-replace.** There is no PATCH: one verb, no
  merge rules to document. A `PUT` on a repo with a job in flight is `409` —
  the job snapshotted its definition at admission, so a replace would silently
  not apply.
- **`branch` collapses create, checkout and set-upstream** into one call.
  `create: false`, `checkout: false` and no `upstream` is `422` ("nothing to
  do").
- **`reset` requires `confirm: true`.** Without it, `422 confirm_required`.
- **There is no cancel endpoint and no file endpoint.** libgit2 is
  interruptible only inside its transfer callbacks, so a cancel would work
  during fetch and silently no-op during merge, checkout and TCP connect;
  it is replaced by `[git].network_timeout_secs` and a visible `stalled` flag
  on the job. And you already have the tree path — proxying file bytes through
  HTTP would double every byte and create a second, worse filesystem API.
````

- [ ] **Step 4: Add §9.4, the repo definition reference**

Append to the end of `README.md`:

````markdown

### 9.4 Defining a repo

`PUT /api/git/repos/{id}`. The id must match `^[a-z0-9][a-z0-9._-]{0,63}$` and
is **lowercase only** — macOS and Windows filesystems are case-insensitive, so
`Notes` and `notes` would collide into one directory on half of all machines.
It must also not be `.`, contain `..`, end in `.` or a space, or be a Windows
reserved name (`con`, `prn`, `aux`, `nul`, `com1`–`com9`, `lpt1`–`lpt9`). The
working tree is always `<data-dir>/repos/<id>/`; the id charset cannot express
`/`, `..` or an absolute path, so it cannot escape.

| field | default | meaning |
|---|---|---|
| `remote` | `null` | `https://`, `ssh://`, `git://`, `file://`, `git@host:path`, or an absolute local path. `http://` is rejected as `insecure_remote` unless `[git].allow_http`; a URL carrying a password in its userinfo is always rejected. `null` means a local-only repo: `sync` inits and commits and reports `outcome: "committed"`, while `clone`/`pull`/`push` fail `remote_missing`. |
| `remote_name` | `"origin"` | `[A-Za-z0-9._-]{1,64}`. |
| `branch` | `[git].default_branch` | The one branch this repo tracks. A mutating op on a different checked-out branch fails `branch_mismatch` rather than guessing. |
| `credential` | `null` | `{"kind":"none"}`, `{"kind":"token","username"?,"token"}`, or `{"kind":"ssh_key","username"?,"private_key_path"\|"private_key","public_key_path"?,"public_key"?,"passphrase"?}`. `username` defaults to `x-access-token` for tokens and `git` for SSH. Exactly one of `private_key_path` / `private_key`. |
| `author` | `null` | `{"name","email"}`. Falls back to `[git].author_name` / `author_email`, then to `[app].name` and `<identifier>@<hostname>`. |
| `sync_settings` | `false` | Two-way sync of `settings.json` through the repo — see §9.6. Requires `menu.settings = true`; a definition that asks for it without a settings schema is rejected (`settings_sync_unavailable`) rather than silently doing nothing. |
| `settings_path` | `"settings.json"` | Where in the tree the settings file lives. Relative, forward slashes, no `..`, no leading `/`. |
| `auto_sync_secs` | `null` | `null` or `0` turns it off. Anything else is clamped into `30…86400`, with an `auto_sync_clamped` warning on the response. |
| `sync_on_start` | `false` | One sync just after the `[server]` child comes up. Fire-and-forget; it never delays the window. |
| `sync_on_quit` | `false` | Sync during Quit, after the children are killed, bounded by `[git].quit_sync_timeout_secs`. |
| `restart_children_on_pull` | `false` | Per-repo default for the per-request `restart_children`. |

The response never contains a secret. A stored credential comes back as
`token_stored: true` / `private_key_stored: true` / `passphrase_stored: true`,
plus a `has_credentials` boolean — there are no secret-shaped fields on the
wire type at all, so you cannot leak one by forgetting an annotation. Key
*paths* are shown verbatim: they are not secrets, and hiding them makes
misconfiguration undebuggable.

A `credential` sent on a **job** body (`clone`, `pull`, `push`, `sync`) wholly
replaces the stored one for that one call — never a field-by-field merge — and
is never written to disk. `{"kind":"none"}` is how you make one call skip a
stored PAT, which is useful for a public HTTPS pull that should not spend a
rate-limited token.
````

- [ ] **Step 5: Add §9.5 — jobs, retries, and the error contract (including the 502-not-401 rule)**

Append to the end of `README.md`:

````markdown

### 9.5 Jobs, retries, and errors

Every mutating call returns `202` immediately with a job body and a
`Location: /api/git/jobs/<job_id>` header. **One job per repo at a time**: a
second call while one is running is `409 repo_busy`, with `Retry-After: 1` and
the running job embedded in `error.job`.

Send a `request_id` (free-form, ≤128 printable ASCII characters) on every
mutating call. It is scoped to the repo, and it is checked *before* the busy
check, so:

- same `request_id` while its job is still **running** → `200` with that same
  job. This is the dropped-response case, and it can never come back as a 409.
- same `request_id` after the job **succeeded** → `200` with that same job. The
  effect already happened.
- same `request_id` after the job **failed** → the index entry is consumed and
  a fresh job is admitted. Retrying a failed operation with the same id is
  meant to work.

Because a matching `request_id` can never produce a 409, a 409 always means
*someone else* holds the repo.

A job body carries `state` (`running` / `succeeded` / `failed`), `phase`
(`preparing`, `cloning`, `staging`, `committing`, `fetching`, `merging`,
`checking_out`, `pushing`, `finalizing`), transfer `progress`, a `stalled`
flag, and on success a `result` with `outcome`, `head_before` / `head_after`,
`merge`, `ahead` / `behind`, `pushed`, and the `settings_*` fields.

Every non-2xx from `/api/git/*` uses one envelope:

```json
{ "error": { "code": "repo_busy",
             "message": "repo \"notes\" is busy running sync",
             "repo_id": "notes",
             "host_instance": "7c1e93aa",
             "job": { "id": "job_7c1e93aa_00002b", "op": "sync",
                      "state": "running", "stalled": false,
                      "started_at_ms": 1785000000205, "request_id": "boot-1" } } }
```

`code` is a stable string — branch on it, never on `message`. `repo_id`,
`path`, `field` and `job` are present only when they apply (`job` only on
`repo_busy`). The pre-existing `/api/settings` and `/api/restart` endpoints
keep their plain-text bodies; the envelope is deliberately not retrofitted onto
them.

> **`auth_failed` is HTTP 502, never 401.** Your request's own `x-host-token`
> authentication *succeeded* — it is the **upstream git remote** that rejected
> the credential. Returning 401 would make a generic "refresh my token and
> retry" middleware do exactly the wrong thing. The same applies to
> `auth_missing`, `auth_unbound`, `host_key_mismatch` and
> `certificate_invalid`: all 502. The only 401 this API ever returns is a
> missing or wrong `x-host-token`.

Jobs live in memory only. Fifty terminal jobs are retained, for one hour, and
none is ever evicted younger than a minute. **`host_instance` is the restart
contract**: eight hex characters minted once per host launch, present in every
job body, in `GET /api/git`, in every `job_not_found` envelope, and as the
middle field of the job id itself. On `job_not_found`:

- the envelope's `host_instance` **differs** from the one you last saw → the
  host restarted. `GET /api/git/repos/{id}`, read `status.last_sync`, and
  re-issue.
- it **matches** → your job was evicted by retention. `status.last_sync` still
  carries the outcome; it lives in `git-state.json` and survives both eviction
  and a restart.

Every operation is idempotent enough for a blind re-run: a commit whose tree
already matches HEAD is a no-op, a push that already landed reports
`up_to_date`, and a clone onto an existing tree reuses it.
````

- [ ] **Step 6: Add §9.6 — automatic syncs, the tray, dialogs, and `sync_settings`**

Append to the end of `README.md`:

````markdown

### 9.6 Automatic syncs, the tray, and dialogs

- **`auto_sync_secs`** runs one sync per repo on a timer. A tick that lands on
  a busy repo is **dropped, not queued** — a backlog of stale syncs is never
  useful and would turn one slow network into an unbounded queue. A `PUT` that
  changes or clears the interval takes effect after at most one old period; a
  `DELETE` lets the timer retire itself.
- **`sync_on_start`** fires after the `[server]` child is up, never before, so
  a wedged network at launch cannot make the app look dead before the first
  window appears.
- **`sync_on_quit`** runs inside Quit *after* the children are killed, so the
  window closes immediately and the wait is invisible. It is bounded by
  `[git].quit_sync_timeout_secs`; setting that to `0` disables the feature
  entirely.
- **`[git].tray_sync = true`** adds a **Sync now** entry to the tray menu,
  immediately above "Restart App", and only while at least one repo is defined.
  It admits one sync per repo, skips busy repos with a log line, and returns
  instantly.
- **`[git].error_dialogs = true`** covers **both** halves of "something you
  should know about happened":
  - a native **error** dialog on a repo's `ok → failing` transition — once per
    outage, not once per tick, so an expired token on a five-minute timer
    cannot stack a modal every five minutes forever;
  - a native **warning** dialog when a sync succeeded but overwrote a remote
    edit (§9.2), naming the files and printing the `git show …^2:<path>`
    recovery command, de-duplicated on the merge commit id so a retried push
    re-reporting the same merge produces one dialog, not two.

  Both are suppressed while the app is in tray mode: a background sync must
  never steal focus from a user who deliberately minimised the app.
- **`restart_children`** (per request, or `restart_children_on_pull` per repo)
  restarts the `[server]` child after a pull that actually moved HEAD. It never
  fires on `up_to_date` — that is what stops a five-minute auto-sync from
  restarting Chrome 288 times a day — it is never set by a timer-driven sync,
  and it is deferred into the job's `warnings` while the app is not yet
  `Ready`. Automatic syncs can never restart the user's window; only an
  explicit HTTP call, or a real `sync_settings` change, can.
- **`sync_settings = true`** copies `<data-dir>/settings.json` into the tree at
  `settings_path` before staging (materialising it from the schema defaults if
  it does not exist yet) and, after the merge, validates whatever came back
  through the same validator the settings page uses:
  - **valid** → it is saved through `settings::save`, `result.settings_synced`
    is `true`, and the children restart only if a normalised value actually
    changed;
  - **invalid** → the local `settings.json` is **left untouched**,
    `result.settings_rejected` explains why, the job still **succeeds**, and
    the host writes its own valid file back into the tree so the next push
    heals the remote. A teammate's typo must not fail an entire git sync
    forever.

  Values round-trip through the normal save path, so key order and formatting
  are normalised and two hosts syncing the same settings do not fight over
  whitespace.
````

- [ ] **Step 7: Add §9.7 — operational notes and how to run the network tests**

Append to the end of `README.md`:

````markdown

### 9.7 Operational notes

- **You own `.gitignore`.** A sync stages everything the repo does not ignore.
  A `node_modules` inside a repo tree *will* be committed. Git's ignore rules
  are honoured and nothing is ever force-added, but the tree is yours by
  design.
- **`repos.json` is not hot-reloaded.** It is read once at startup and
  rewritten only by `PUT` / `DELETE` — never by a sync, a timer or a job. Hand-
  edit it while the app is closed. Entries the host cannot parse are skipped,
  reported in `registry_error`, and **written back verbatim** on every
  subsequent rewrite, so a typo never costs you the rest of your file. A file
  that fails to parse at all is renamed to `repos.json.corrupt-<epoch_ms>` and
  the host still starts.
- **Everything is logged** to `<data-dir>/logs/git.log`, one line per outcome,
  rotating at 5 MB into `git.log.old`:

  ```
  <epoch_ms> <op> repo=<id> job=<job_id> ok  code=-      <message>
  <epoch_ms> <op> repo=<id> job=<job_id> err code=<code> <message>
  ```

  URLs are stripped of their userinfo and stored secrets are replaced with
  `***` before anything is written.
- **Windows.** `<data-dir>/repos/<id>/` plus a deep repository path can cross
  `MAX_PATH`, so keep ids short. A filename that is not valid UTF-8 produces an
  explicit `io_failed` rather than a silently mangled path.
- **Network integration tests.** Four `#[ignore]`d tests in `src/git/mod.rs`
  cover what an offline CI cannot. Run them by hand:

  ```bash
  # No credentials needed: the vendored-TLS canary and the credential retry cap.
  cargo test --bin chrome-host-app git::tests:: -- --ignored --nocapture

  # The credentialed pair, against a scratch repository you own.
  GIT_TEST_HTTPS_URL=https://github.com/you/scratch.git \
  GIT_TEST_TOKEN=github_pat_... \
  GIT_TEST_SSH_URL=git@github.com:you/scratch.git \
  GIT_TEST_SSH_KEY=$HOME/.ssh/id_ed25519 \
  cargo test --bin chrome-host-app git::tests:: -- --ignored --nocapture
  ```

  The scratch repository must be non-empty and have a `main` branch; the HTTPS
  test pushes a commit to it. `GIT_TEST_SSH_KEY` must be an unencrypted private
  key. CI runs `cargo test --locked` with no secrets and no network, so none of
  these ever runs there.
- **Don't need git at all?** See the removal recipe at the end of §7.
````

- [ ] **Step 8: Verify §9 is complete and internally consistent**

Run:

```bash
grep -n '^### 9\.[0-9]' README.md
grep -c 'auth_failed` is HTTP 502, never 401' README.md
grep -c 'error_dialogs' README.md
sed -n '/^## 9\. Git service/,$p' README.md | grep -n 'TODO\|TBD\|FIXME'
```

Expected: seven `### 9.x` headings (`9.1 Quickstart`, `9.2 The merge policy…`,
`9.3 Endpoints`, `9.4 Defining a repo`, `9.5 Jobs, retries, and errors`,
`9.6 Automatic syncs, the tray, and dialogs`, `9.7 Operational notes`); `1` for
the 502 rule; `3` for `error_dialogs` (§3 table, §9.2, §9.6); and **no output at
all** from the last grep (grep exits 1 — that is the pass condition).

The last grep is scoped to §9 onward on purpose: §7 already contains the
sentence "Code signing and notarization … are a per-app TODO", which is
deliberate prose telling the template user what they still owe. Grepping the
whole file could therefore never pass.

- [ ] **Step 9: Commit §9**

```bash
git add README.md
git commit -m "docs: add README §9, the git service reference

Opens with a runnable Node quickstart (define -> sync -> poll -> use
repo.path with ordinary fs calls) before any endpoint table, because a
template user has to see the shape in thirty seconds or they will not read
the rest. Says in bold that the prefer-local merge policy discards remote
edits and is wrong for repos with concurrent human editors, and documents
the one surprising status code: an upstream credential rejection is 502,
never 401, so a generic token-refresh middleware does not misfire."
```

---

## Part B — README §2–§7 updates

- [ ] **Step 10: Add the `[git]` key table to §3**

Insert into `README.md` immediately after the `[[settings.fields]]` table (which ends at
`README.md:113`) and immediately before `## 4. Local server contract`:

````markdown
### `[git]` (optional; entire section omitted by default)

Present only if you want the host to manage git repositories on behalf of the
app. **The presence of the section — even completely empty — is the on switch**,
exactly like `[server]`; there is no `enabled` key. While it is absent the whole
subsystem is inert: no directory is created, `repos.json` is never written, no
libgit2 code runs, and every `/api/git/*` route answers `404 git_disabled`.
Full documentation in §9.

| key | type | default | behavior |
|---|---|---|---|
| `tray_sync` | bool | `false` | Adds a "Sync now" entry to the tray menu, immediately above "Restart App". It only appears once at least one repo is defined. |
| `error_dialogs` | bool | `false` | Show a native dialog when a repo's sync starts failing, **and** when a sync succeeded but overwrote a remote edit (§9.2). Both are suppressed while the app is in tray mode. |
| `status_api` | bool | `false` | Adds a `git` key to `GET /api/status` summarising each repo's last sync. Off by default so the response stays byte-identical to a host with no `[git]` section. |
| `registry_writes` | bool | `true` | `false` makes `<data-dir>/repos.json` author-owned: `PUT` and `DELETE /api/git/repos/{id}` answer `403 registry_read_only`, and the app can only sync the repos you shipped. |
| `default_branch` | string | `"main"` | Branch used by repo definitions that do not name one. Must be a valid branch name: non-empty, ≤ 200 bytes, only `[A-Za-z0-9._/-]`, no `..`, no `//`, no leading `-` or `/`, no trailing `/`, not ending in `.lock`, not exactly `@`, no ASCII control characters. |
| `author_name` | string | `""` | Commit author name. Empty means `[app].name`. Must not contain `<`, `>` or newlines — a git signature cannot represent them. |
| `author_email` | string | `""` | Commit author email. Empty means `<identifier>@<hostname>`. If set, must contain `@` and no `<`, `>` or whitespace. |
| `network_timeout_secs` | integer | `120` | libgit2's per-connection idle timeout and the deadline every fetch/push job runs against. Must be between 5 and 3600. |
| `quit_sync_timeout_secs` | integer | `10` | Upper bound on how long Quit waits for repos with `sync_on_quit`. `0` disables `sync_on_quit` entirely. Must be 120 or less. |
| `allow_http` | bool | `false` | Permit plaintext `http://` remotes. Left `false`, an `http://` remote is rejected with `insecure_remote`. |
| `ssh_host_key_policy` | string: `"tofu"` \| `"accept"` | `"tofu"` | `"tofu"` pins a remote's SSH host key the first time it is seen (into `git-state.json`) and fails `host_key_mismatch` if it ever changes. `"accept"` takes any host key — only for a network you control. |

These are validated at load time whether or not `[git]` is ever used, so
enabling a repo never surfaces a new config error at an awkward moment.

````

- [ ] **Step 11: Document the host-access environment variables in §4**

First, extend §4's numbered item 2 (`README.md:125-129`). Change:

```
2. Builds the child's environment by layering, in order: `[server].env`
   from `app.toml`, then one `APP_SETTING_<KEY>` variable per settings
   field (uppercased key, current value; see §5), then
   `APP_SETTINGS_FILE=<path to settings.json>` (always present, even
   when no settings schema is configured).
```

to:

```
2. Builds the child's environment by layering, in order: `[server].env`
   from `app.toml`, then one `APP_SETTING_<KEY>` variable per settings
   field (uppercased key, current value; see §5), then
   `APP_SETTINGS_FILE=<path to settings.json>` (always present, even
   when no settings schema is configured), then the host-access variables
   described below.
```

Then insert a new subsection immediately before `## 5. Settings`:

````markdown
### Host access variables

Every `[server]` child also receives these, so it can call the host's own
loopback API:

| variable | value | present |
|---|---|---|
| `APP_HOST_URL` | `http://127.0.0.1:<port>` — the host's internal server, on a port chosen fresh at every launch | always |
| `APP_HOST_TOKEN` | the random per-launch token to send as the `x-host-token` header | always |
| `APP_GIT_ENABLED` | `"1"` | only when `[git]` is present in `app.toml`. When it is not, the variable is **absent** — not `"0"` |

`APP_GIT_ENABLED` is how a child feature-detects the git service (§9) without
paying a round trip. Chrome never receives any of these: `chrome::launch`
inherits the host's own environment, and this vector is built only for the
`[server]` child.

````

- [ ] **Step 12: Add the credential-storage security delta to §5**

Append to the end of `## 5. Settings`, immediately before `## 6. Data locations`:

````markdown
**Enabling `[git]` widens that blast radius, so read this before you do.**
Without `[git]` the host holds no credentials at all. With it, the host may
hold a personal access token, an SSH passphrase, or an inline private key in
`<data-dir>/repos.json`. The loopback token still protects only against other
**web pages**, not against other **processes** run by the same user — anything
running as you can fetch `/loading`, read the token out of the HTML, and drive
every git endpoint. Three consequences worth acting on:

- **Prefer a fine-grained PAT scoped to the single repository the app needs.**
  Nothing can read a stored secret back out over HTTP — the secret type has no
  serializer and the repo JSON carries only `token_stored: true` — but a
  loopback caller can still *use* a stored credential to push, so scope what it
  can reach.
- **A stored credential is only ever sent to the host it was stored against.**
  Its `bound_host` is recorded when you save it. A `PUT` that repoints the
  remote at a different host **drops** the credential and returns an
  `auth_unbound` warning, and an operation that would need an unbound
  credential fails `auth_unbound` rather than transmitting it. That closes the
  worst primitive the feature would otherwise create: repoint a repo at an
  attacker's host, trigger a fetch, receive the user's PAT.
- **If your repos are author-declared, set `registry_writes = false`.** The app
  can then only sync the remotes you shipped; `PUT` and `DELETE` answer
  `403 registry_read_only`.

Two structural properties back this up. `repos.json` is created `0600` on unix
and tightened in place if found looser (on Windows it inherits the `%APPDATA%`
ACL — documented, not enforced). And no `git` subprocess is ever spawned:
libgit2 is linked into the binary, so repository hooks, `core.sshCommand`,
`GIT_SSH_COMMAND`, credential helpers, aliases and external clean/smudge
filters are all inert — a materially stronger position than shelling out to
`git`.

What is **not** solved: the token is still readable by any process running as
the same user. Closing that needs OS-level peer authentication (a unix socket
with `SO_PEERCRED`, a named pipe with an ACL) replacing the TCP listener
entirely, which is out of scope for this template.

````

- [ ] **Step 13: Add the git paths to the §6 data-locations table**

In `## 6. Data locations`, replace the `logs/` row (`README.md:187`) with one that
mentions `git.log`, and add three rows after `app.lock` (`README.md:189`). The
resulting table body:

````markdown
| path | contents |
|---|---|
| `chrome-profile/` | Chrome's dedicated user-data-dir for this app — kept separate from the user's normal Chrome profile. |
| `logs/` | `server.log` (+ `.old`) for the supervised `[server]` child, `host.log` (+ `.old`) for the host itself, `chrome.log` (+ `.old`) for Chrome's own stdout/stderr (GPU probes, service chatter — kept out of the host's terminal), and `git.log` (+ `.old`) for the git service (§9) when `[git]` is enabled. All rotate at 5 MB. |
| `settings.json` | Persisted settings values, keyed by settings field `key`. |
| `app.lock` | Single-instance lock file; prevents two copies of the app running against the same identifier at once. |
| `repos/` | One working tree per defined repo, at `repos/<id>/`. Each is an ordinary git repository you can `cd` into and run `git` in. Created by the git service on first use; never created when `[git]` is absent. |
| `repos.json` | Git repo definitions, **including credentials**. `0600` on unix. Written only by `PUT`/`DELETE /api/git/repos/{id}` — never by a sync, a timer or a job. Read once at startup; not hot-reloaded. A file that fails to parse is renamed to `repos.json.corrupt-<epoch_ms>` and never deleted. |
| `git-state.json` | Host-owned volatile git state: each repo's last sync outcome and its pinned SSH host fingerprint. Rewritten after every finished job. Never contains a secret; safe to delete, at the cost of the last-sync record and the host-key pin. |
````

- [ ] **Step 14: Add `perl`, `make` and `pkg-config` to the §2 prerequisites**

In `## 2. Quick start` (`README.md:31-33`), replace:

```bash
sudo apt-get install libgtk-3-dev libayatana-appindicator3-dev libxdo-dev
```

with:

```bash
sudo apt-get install libgtk-3-dev libayatana-appindicator3-dev libxdo-dev \
                     perl make pkg-config
```

and add this sentence immediately after that block, before the "and, at runtime"
paragraph:

```
  `perl`, `make` and `pkg-config` are for the vendored OpenSSL that the git
  service's dependencies build from source — see §7 for why, and for how to
  opt out.
```

- [ ] **Step 15: Rewrite the §7 build-prerequisite table and add the vendored-OpenSSL notes**

In `## 7. Building and packaging`, replace the three-row prerequisite table
(`README.md:208-212`) with:

````markdown
| OS | toolchain | extra build dependencies |
|---|---|---|
| Linux (Debian/Ubuntu) | Rust 1.85+ via [rustup](https://rustup.rs) | `sudo apt-get install libgtk-3-dev libayatana-appindicator3-dev libxdo-dev perl make pkg-config` (same list as §2) |
| macOS | Rust 1.85+ via rustup, plus Xcode Command Line Tools (`xcode-select --install`) | **`perl` and `make`** — both ship with the Command Line Tools, so in practice nothing extra to install |
| Windows | Rust 1.85+ via rustup with the default MSVC toolchain (requires [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) with "Desktop development with C++") | none — MSVC only. No perl, no make, no OpenSSL |
````

Then insert these subsections immediately after the "Google Chrome is a **runtime**
requirement…" paragraph (`README.md:214-218`) and before `### Installers`:

````markdown
### Why perl and make

`git2` is built with `vendored-libgit2`, and on **every** unix target — macOS
included — libssh2 pulls in `openssl-sys` with the `vendored` feature. That
compiles OpenSSL from source, and OpenSSL's build system is written in perl and
driven by make. A machine with a C toolchain but no perl fails its very first
`cargo build` with an opaque `openssl-src` error, which is why perl is called
out here rather than assumed.

The common belief that macOS needs no OpenSSL is wrong: libgit2's own HTTPS
backend there is SecureTransport, but **libssh2's crypto is OpenSSL on macOS
too**.

|  | Linux | macOS | Windows MSVC |
|---|---|---|---|
| libgit2 HTTPS backend | OpenSSL | SecureTransport | WinHTTP |
| libssh2 crypto | OpenSSL | OpenSSL | WinCNG |
| `openssl-sys` in the dependency graph | yes | **yes (via libssh2)** | no |
| extra build prerequisites | perl, make, pkg-config | perl, make | none |

CA certificates need no configuration: libgit2's initialiser probes the
system's certificate locations for us on Linux.

The cost is roughly **+80 seconds on a clean build** per OS (Windows is the
fast leg, having no OpenSSL to build) and about **+7.5 MB** on the stripped
release binary. The payoff is a `.deb`/`.dmg`/`.msi` that links only libc and
needs no system libgit2, libssh2 or OpenSSL at runtime.

**Distro packagers** who would rather link the system libraries can opt out:

```bash
LIBGIT2_NO_VENDOR=1 LIBSSH2_SYS_USE_PKG_CONFIG=1 cargo build --release
```

This needs `libgit2-dev`, `libssh2-1-dev` and `libssl-dev` (or the equivalents)
installed and discoverable by `pkg-config`. The trade-off is real in both
directions: the binary then tracks the distro's OpenSSL for CVE fixes instead
of needing a rebuild, but it stops being self-contained.

**CI caching.** Both workflows use `Swatinem/rust-cache@v2`, which absorbs the
80 seconds after the first run because it caches the build directory as well as
the registry. If you replace it with a hand-rolled `actions/cache`, caching
`~/.cargo/registry` alone saves **none** of this cost — the expensive artifacts
are compiled C static libraries, and the cache must cover
`target/*/build/{libgit2-sys,libssh2-sys,openssl-sys}-*`.

### Removing the git service

There is no cargo feature gate for git — one binary, one configuration, because
a compile-time axis doubles the CI matrix and creates "compiles for me" bugs.
If you do not need it and want the build time and the 7.5 MB back, it comes out
in three edits:

1. delete `src/git/`;
2. delete the `mod git;` line from `src/main.rs`;
3. delete the `git2` and `gethostname` dependencies and the whole
   `[target.'cfg(unix)'.dependencies]` block from `Cargo.toml`.

Leaving `[git]` out of `app.toml` already makes the subsystem completely inert
at runtime; this only reclaims build cost.

````

- [ ] **Step 16: Verify the §2–§7 updates**

Run:

```bash
for p in 'perl make pkg-config' 'LIBGIT2_NO_VENDOR=1' 'APP_GIT_ENABLED' \
         'git-state.json' 'registry_writes' 'error_dialogs'; do
  printf '%-24s %s\n' "$p" "$(grep -c "$p" README.md)"
done
grep -c 'target/\*/build/{libgit2-sys,libssh2-sys,openssl-sys}-\*' README.md
grep -c '^### `\[git\]`' README.md
```

Expected — **exact** counts, measured against the landed README. A range cannot
fail usefully, so any deviation is a real drift to go and look at:

```
perl make pkg-config     2      # §2 install block, §7 table
LIBGIT2_NO_VENDOR=1      1      # §7 "Removing the git service"
APP_GIT_ENABLED          2      # §4 table row, §4 explanation
git-state.json           3      # §6 row, §9.5, §9.7
registry_writes          3      # §3 table, §5 delta, §9.4
error_dialogs            3      # §3 table, §9.2, §9.6
1                               # the build-cache path
1                               # the [git] subheading
```

("Why perl and make" names the two tools in prose rather than as the literal
`perl make pkg-config` string, which is why that count is 2 and not 3.)

- [ ] **Step 17: Commit the §2–§7 updates**

```bash
git add README.md
git commit -m "docs: thread the git service through README §2-§7

The [git] key table, the APP_HOST_URL/APP_HOST_TOKEN/APP_GIT_ENABLED child
env contract, the credential-storage security delta (the loopback token
guards against other web pages, not other processes — prefer a fine-grained
PAT, and set registry_writes=false for author-declared repos), the
repos.json / git-state.json / repos/<id>/ data rows, and the build
prerequisites. perl and make are required on Linux AND macOS, not just
Linux: libssh2 pulls in vendored OpenSSL on every unix target. Windows is
MSVC-only. LIBGIT2_NO_VENDOR=1 is documented as the packager escape hatch."
```

---

## Part C — CI

- [ ] **Step 18: Add `perl` and `pkg-config` to both workflows' Linux apt line**

In `.github/workflows/ci.yml`, replace lines 16-24:

```yaml
      - name: Install Linux system deps
        if: runner.os == 'Linux'
        run: |
          sudo apt-get update
          sudo apt-get install -y libgtk-3-dev libayatana-appindicator3-dev libxdo-dev
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      - uses: Swatinem/rust-cache@v2
```

with:

```yaml
      # perl and pkg-config are for the vendored OpenSSL that libssh2-sys pulls
      # in on every unix target (git2 = vendored-libgit2 + ssh). make already
      # comes with build-essential on the GitHub runners. macos-latest and
      # windows-latest need nothing extra: macOS gets perl and make from the
      # Xcode CLT, and Windows MSVC builds no OpenSSL at all.
      - name: Install Linux system deps
        if: runner.os == 'Linux'
        run: |
          sudo apt-get update
          sudo apt-get install -y libgtk-3-dev libayatana-appindicator3-dev libxdo-dev \
                                  perl pkg-config
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      # Caches ~/.cargo *and* ./target, which is what matters here: the ~80 s
      # this build costs is compiling libgit2/libssh2/OpenSSL into
      # target/*/build/{libgit2-sys,libssh2-sys,openssl-sys}-*. A cache of
      # ~/.cargo/registry alone would save none of it.
      - uses: Swatinem/rust-cache@v2
```

Apply the identical two edits to `.github/workflows/release.yml` lines 24-30 (same
apt block, same comment above `Swatinem/rust-cache@v2`; that workflow's
`dtolnay/rust-toolchain@stable` step has no `components:` key — leave it alone).

- [ ] **Step 19: Verify both workflows still parse and carry the new packages**

Run:

```bash
grep -n 'perl pkg-config' .github/workflows/ci.yml .github/workflows/release.yml
python3 -c "import yaml,sys;[yaml.safe_load(open(f)) for f in sys.argv[1:]];print('yaml ok')" \
  .github/workflows/ci.yml .github/workflows/release.yml
```

Expected: one hit per file from the grep, then `yaml ok`. If PyYAML is not
installed, `pip install pyyaml` or skip the second command and instead check the
line continuation by eye — the `\` must be the last character on its line, and the
continuation must be indented under it inside the `run: |` block.

- [ ] **Step 20: Commit the CI change**

```bash
git add .github/workflows/ci.yml .github/workflows/release.yml
git commit -m "ci: install perl and pkg-config for the vendored OpenSSL build

git2's ssh feature pulls libssh2-sys, which declares a non-optional
openssl-sys dependency on every cfg(unix) target. Vendored OpenSSL is built
by perl and make, so a runner with only a C toolchain fails with an opaque
openssl-src error. Also documents why rust-cache is load-bearing here: the
cost lives in target/*/build/{libgit2-sys,libssh2-sys,openssl-sys}-*, not
in ~/.cargo/registry."
```

---

## Part D — the four `#[ignore]`d network tests

> **On the missing red phase.** These four tests cannot have one. Every API they
> call already exists and works (tasks 1–11 landed it), and they are `#[ignore]`d,
> so `cargo test` neither runs nor fails them. The genuine failure signal is
> compilation: a wrong type or a wrong field name shows up the moment you run the
> `--list` step. Each cycle is therefore: write it → prove it compiles and is
> registered as ignored → run it for real against the network → commit.

- [ ] **Step 21: Add the harness and `vendored_tls_trusts_public_ca` to `src/git/mod.rs`**

Append inside the existing `#[cfg(test)] mod tests { … }` block at the bottom of
`src/git/mod.rs`, after the `git2::Version` test that task 1 put there. If that
module already has `use super::*;`, leave it — the explicit `use super::{…}` below
is legal alongside a glob and shadows it.

```rust
    // ── network integration tests ─────────────────────────────────────────
    // The only tests in this crate that touch a real remote, and therefore the
    // only #[ignore]d ones here. They live in mod.rs rather than in ops.rs or
    // creds.rs because each exercises the whole GitService -> jobs -> ops ->
    // creds path; none is a unit test of a single file. CI runs
    // `cargo test --locked` with no secrets and no network.

    use super::{GitService, StartOutcome};
    use crate::config::{AppConfig, RuntimePaths};
    use crate::git::jobs::{JobOp, JobState, JobView};
    use crate::git::ops::OpRequest;
    use crate::git::registry::{CredentialSpec, RepoDef};
    use crate::git::secret::Secret;
    use crate::git::state::StateStore;
    use crate::internal_server::{AppStatus, HostEvent};
    use std::path::Path;
    use std::sync::Arc;

    const TEST_TOML: &str = r#"
[app]
name = "GitNetTest"
identifier = "com.example.gitnettest"

[git]
default_branch = "main"
network_timeout_secs = 60
"#;

    /// Owns the tokio runtime and the event receiver for as long as the test
    /// runs. Dropping the runtime would cancel the `spawn_blocking` the job is
    /// executing on, and `run_job` below would then spin until its deadline on
    /// a job that no longer has a worker.
    struct Harness {
        #[allow(dead_code)]
        rt: tokio::runtime::Runtime,
        #[allow(dead_code)]
        events: tokio::sync::mpsc::UnboundedReceiver<HostEvent>,
        paths: RuntimePaths,
        svc: Arc<GitService>,
    }

    fn harness(base: &Path) -> Harness {
        let cfg = AppConfig::from_str(TEST_TOML).expect("test config parses");
        let paths = RuntimePaths::under(base, &cfg.app.identifier);
        paths.ensure().expect("create data dirs");
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let (tx, events) = tokio::sync::mpsc::unbounded_channel();
        // Ready, so `after_job` is not in its "defer the restart" branch.
        let status = Arc::new(tokio::sync::RwLock::new(AppStatus::Ready {
            target_url: "http://127.0.0.1:1/".to_string(),
        }));
        let svc = GitService::new(&cfg, &paths, rt.handle().clone(), tx, status)
            .expect("[git] is present in TEST_TOML, so the service exists");
        svc.start();
        Harness {
            rt,
            events,
            paths,
            svc,
        }
    }

    fn def(id: &str, remote: &str, credential: Option<CredentialSpec>) -> RepoDef {
        RepoDef {
            id: id.to_string(),
            remote: Some(remote.to_string()),
            remote_name: "origin".to_string(),
            // "" and 0 are the sentinels Registry::put normalises: the branch
            // is filled from [git].default_branch, the timestamps from now.
            branch: String::new(),
            credential,
            author: None,
            sync_settings: false,
            settings_path: "settings.json".to_string(),
            auto_sync_secs: None,
            sync_on_start: false,
            sync_on_quit: false,
            restart_children_on_pull: false,
            created_at_ms: 0,
            updated_at_ms: 0,
        }
    }

    /// Admit a job and block until it is terminal. Sleeping is legal here and
    /// nowhere else in this crate: these tests wait on a real network, so the
    /// zero-sleep rule the `jobs.rs` tests hold to does not apply.
    fn run_job(h: &Harness, id: &str, op: JobOp) -> JobView {
        let slot = match h
            .svc
            .start_job(id, op, OpRequest::manual())
            .expect("job admitted")
        {
            StartOutcome::Started(s) | StartOutcome::Replay(s) | StartOutcome::Busy(s) => s,
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
        while !slot.is_terminal() {
            assert!(
                std::time::Instant::now() < deadline,
                "job {} never reached a terminal state",
                slot.id
            );
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        slot.view(h.svc.host_instance())
    }

    fn code_of(view: &JobView) -> &'static str {
        view.error.as_ref().map(|e| e.code.as_str()).unwrap_or("-")
    }

    fn message_of(view: &JobView) -> Option<String> {
        view.error.as_ref().map(|e| e.message.clone())
    }

    /// The canary for the vendored-TLS story. A `vendored-libgit2` +
    /// vendored-OpenSSL build has to find the machine's CA bundle by itself
    /// (libgit2's initialiser probes for it). If this fails on a distro, every
    /// `https://` remote fails on that distro — so run it manually on every
    /// platform the app ships to.
    #[test]
    #[ignore = "requires network; run with cargo test -- --ignored"]
    fn vendored_tls_trusts_public_ca() {
        let tmp = tempfile::tempdir().unwrap();
        let h = harness(tmp.path());
        h.svc
            .put_repo(
                "libc",
                def("libc", "https://github.com/rust-lang/libc.git", None),
            )
            .expect("define repo");

        let view = run_job(&h, "libc", JobOp::Clone);
        assert_eq!(
            view.state,
            JobState::Succeeded,
            "clone failed [{}]: {:?}",
            code_of(&view),
            message_of(&view)
        );
        assert!(
            h.svc.tree_path("libc").join(".git").is_dir(),
            "clone reported success but left no .git directory"
        );
    }
```

- [ ] **Step 22: Prove it compiles, is registered as ignored, and does not run by default**

Run:

```bash
cargo test --bin chrome-host-app -- --ignored --list
cargo test --bin chrome-host-app
```

Expected: the first command lists `git::tests::vendored_tls_trusts_public_ca: test`
alongside the pre-existing `chrome::tests::real_chrome_reports_version: test`. The
second finishes with `test result: ok. N passed; 0 failed; 2 ignored` — the new test
did **not** run, and no existing test changed.

If instead you see a compile error such as
`error[E0609]: no field 'git_state_file' on type 'RuntimePaths'` or
`error[E0061]: this function takes 5 arguments`, an earlier task's API drifted from
the contract — fix the call site here, do not change the earlier task's signature.

- [ ] **Step 23: Run it against the real network**

Run:

```bash
cargo test --bin chrome-host-app git::tests::vendored_tls_trusts_public_ca \
  -- --ignored --exact --nocapture
```

Expected: `test git::tests::vendored_tls_trusts_public_ca ... ok` after roughly
30–120 seconds (it clones a real ~50 MB repository). A failure of
`clone failed [certificate_invalid]` or `[network_failed]` on a machine with
working networking is the signal that the vendored OpenSSL build cannot locate
this platform's CA store.

- [ ] **Step 24: Commit**

```bash
git add src/git/mod.rs
git commit -m "test(git): add the vendored-TLS CA canary

#[ignore]d, like chrome::tests::real_chrome_reports_version. A
vendored-libgit2 + vendored-OpenSSL build has to find the platform CA store
on its own; if it cannot, every https:// remote fails on that platform and
nothing offline would catch it. Also lands the shared network-test harness."
```

- [ ] **Step 25: Add `real_https_clone_and_push_and_server_rejection`**

Append inside the same `mod tests` block, after the previous test:

```rust
    /// The whole HTTPS + stored-credential path against a real server,
    /// including a *server-side* rejection. That last part cannot be covered
    /// offline: libgit2's local transport does not run server hooks, so the
    /// bare-repo fixtures in `ops.rs` can produce a client-side non-fast-
    /// forward but never a `push_update_reference` status line from a server.
    ///
    /// `GIT_TEST_HTTPS_URL` must be a scratch repository you own that already
    /// has a `main` branch with at least one commit; this test pushes to it.
    #[test]
    #[ignore = "requires GIT_TEST_HTTPS_URL and GIT_TEST_TOKEN"]
    fn real_https_clone_and_push_and_server_rejection() {
        let (url, token) = match (
            std::env::var("GIT_TEST_HTTPS_URL"),
            std::env::var("GIT_TEST_TOKEN"),
        ) {
            (Ok(u), Ok(t)) => (u, t),
            _ => panic!("set GIT_TEST_HTTPS_URL and GIT_TEST_TOKEN"),
        };
        let cred = CredentialSpec::Token {
            username: "x-access-token".to_string(),
            token: Secret::new(token.clone()),
            bound_host: None,
        };
        let tmp = tempfile::tempdir().unwrap();
        let h = harness(tmp.path());

        // Two independent clones of the same remote, so one can get ahead of
        // the other without a second process.
        for id in ["alpha", "beta"] {
            h.svc
                .put_repo(id, def(id, &url, Some(cred.clone())))
                .expect("define repo");
            let view = run_job(&h, id, JobOp::Clone);
            assert_eq!(
                view.state,
                JobState::Succeeded,
                "{id} clone failed [{}]: {:?}",
                code_of(&view),
                message_of(&view)
            );
        }

        // alpha advances the remote.
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        std::fs::write(
            h.svc.tree_path("alpha").join("host-test.txt"),
            format!("{stamp}\n"),
        )
        .unwrap();
        let view = run_job(&h, "alpha", JobOp::Sync);
        assert_eq!(
            view.state,
            JobState::Succeeded,
            "alpha sync failed [{}]: {:?}",
            code_of(&view),
            message_of(&view)
        );
        let result = view.result.as_ref().expect("a succeeded job has a result");
        assert_eq!(result["pushed"], serde_json::Value::Bool(true));

        // Leak canary: a stored PAT must not appear anywhere on the wire type.
        let body = serde_json::to_string(&view).unwrap();
        assert!(!body.contains(&token), "the job view leaked the token");

        // beta is now one commit behind, so its push must be refused.
        std::fs::write(
            h.svc.tree_path("beta").join("host-test.txt"),
            "divergent\n",
        )
        .unwrap();
        let view = run_job(&h, "beta", JobOp::Commit);
        assert_eq!(
            view.state,
            JobState::Succeeded,
            "beta commit failed [{}]: {:?}",
            code_of(&view),
            message_of(&view)
        );
        let view = run_job(&h, "beta", JobOp::Push);
        assert_eq!(
            view.state,
            JobState::Failed,
            "a diverged push was accepted — nothing rejected it"
        );
        // Either end may catch it first: libgit2 can refuse a non-fast-forward
        // refspec locally, or the server can answer with a rejection status.
        assert!(
            matches!(code_of(&view), "push_rejected" | "not_fast_forward"),
            "unexpected rejection code {}: {:?}",
            code_of(&view),
            message_of(&view)
        );
    }
```

- [ ] **Step 26: Prove it compiles and is skipped without the environment**

Run:

```bash
cargo test --bin chrome-host-app -- --ignored --list
cargo test --bin chrome-host-app
```

Expected: `git::tests::real_https_clone_and_push_and_server_rejection: test` now
appears in the ignored list, and the default run still ends
`test result: ok. N passed; 0 failed; 3 ignored`.

- [ ] **Step 27: Run it against a real scratch repository**

Run:

```bash
GIT_TEST_HTTPS_URL=https://github.com/you/scratch.git \
GIT_TEST_TOKEN=github_pat_... \
cargo test --bin chrome-host-app git::tests::real_https_clone_and_push_and_server_rejection \
  -- --ignored --exact --nocapture
```

Expected: `test ... ok` in 10–40 seconds, and one new commit visible on the scratch
repository's `main`. If it fails at `beta clone failed [auth_failed]`, the PAT lacks
`contents: write` on that repository. If it fails at
`a diverged push was accepted`, the remote is not refusing non-fast-forwards —
check that `main` is not a mirror or an empty repository.

- [ ] **Step 28: Commit**

```bash
git add src/git/mod.rs
git commit -m "test(git): add the real HTTPS clone/push/rejection test

Covers what the offline fixtures structurally cannot: libgit2's local
transport never runs server hooks, so a push_update_reference rejection
status can only come from a real server. Two clones of one remote diverge
inside a single process, and a leak canary asserts the stored PAT never
appears on the serialized job."
```

- [ ] **Step 29: Add `real_ssh_clone_pins_the_host_key`**

Append inside the same `mod tests` block:

```rust
    /// TOFU host-key pinning, end to end: the first SSH clone learns the
    /// remote's key into `git-state.json`, and a second host whose pin does
    /// not match refuses to talk to that remote at all.
    ///
    /// `GIT_TEST_SSH_KEY` is the path to an *unencrypted* private key with
    /// access to `GIT_TEST_SSH_URL`.
    #[test]
    #[ignore = "requires GIT_TEST_SSH_URL and GIT_TEST_SSH_KEY"]
    fn real_ssh_clone_pins_the_host_key() {
        let (url, key) = match (
            std::env::var("GIT_TEST_SSH_URL"),
            std::env::var("GIT_TEST_SSH_KEY"),
        ) {
            (Ok(u), Ok(k)) => (u, k),
            _ => panic!("set GIT_TEST_SSH_URL and GIT_TEST_SSH_KEY"),
        };
        let cred = CredentialSpec::SshKey {
            username: "git".to_string(),
            private_key_path: Some(std::path::PathBuf::from(&key)),
            private_key: None,
            public_key_path: None,
            public_key: None,
            passphrase: None,
            bound_host: None,
        };
        let tmp = tempfile::tempdir().unwrap();

        // First host: clones, and learns the fingerprint on the way.
        let state_file = {
            let h = harness(tmp.path());
            h.svc
                .put_repo("sshrepo", def("sshrepo", &url, Some(cred)))
                .expect("define repo");
            let view = run_job(&h, "sshrepo", JobOp::Clone);
            assert_eq!(
                view.state,
                JobState::Succeeded,
                "ssh clone failed [{}]: {:?}",
                code_of(&view),
                message_of(&view)
            );
            // Clear the tree so the second host has to reach the network again.
            std::fs::remove_dir_all(h.svc.tree_path("sshrepo")).unwrap();
            h.paths.git_state_file.clone()
        };

        let learned = StateStore::load(&state_file)
            .fingerprint("sshrepo")
            .expect("TOFU recorded a fingerprint");
        assert!(
            learned.starts_with("SHA256:"),
            "unexpected fingerprint format: {learned}"
        );

        // Poison the pin. A fresh host must now refuse the same remote, which
        // is the whole point of pinning: repos.json survives, only the pin
        // changed.
        StateStore::load(&state_file)
            .record_fingerprint("sshrepo", &format!("SHA256:{}", "A".repeat(43)));

        let h = harness(tmp.path());
        let view = run_job(&h, "sshrepo", JobOp::Clone);
        assert_eq!(
            view.state,
            JobState::Failed,
            "a mismatched host-key pin was accepted"
        );
        assert_eq!(code_of(&view), "host_key_mismatch", "{:?}", message_of(&view));
    }
```

- [ ] **Step 30: Compile it, then run it with a real key**

Run:

```bash
cargo test --bin chrome-host-app -- --ignored --list
GIT_TEST_SSH_URL=git@github.com:you/scratch.git \
GIT_TEST_SSH_KEY=$HOME/.ssh/id_ed25519 \
cargo test --bin chrome-host-app git::tests::real_ssh_clone_pins_the_host_key \
  -- --ignored --exact --nocapture
```

Expected: the list now shows four ignored tests (three `git::tests::*` plus
`chrome::tests::real_chrome_reports_version`), then
`test git::tests::real_ssh_clone_pins_the_host_key ... ok` in 5–20 seconds. A
failure at `TOFU recorded a fingerprint` means the certificate-check callback never
ran — check that `[git].ssh_host_key_policy` is left at its `"tofu"` default (it is,
in `TEST_TOML`). A failure at `a mismatched host-key pin was accepted` means the
pinned value is not being compared, which is a real security bug in `creds.rs`.

- [ ] **Step 31: Commit**

```bash
git add src/git/mod.rs
git commit -m "test(git): add the SSH TOFU host-key pinning test

Clones over SSH, asserts the learned SHA256 fingerprint landed in
git-state.json, then poisons the pin and asserts a fresh host refuses the
same remote with host_key_mismatch. Nothing offline can produce a real host
key, so this is the only proof the pin is compared rather than just stored."
```

- [ ] **Step 32: Add `wrong_token_fails_once`**

Append inside the same `mod tests` block:

```rust
    /// libgit2 re-invokes the credentials callback up to fifteen times before
    /// giving up ("too many redirects or authentication replays"), retrying the
    /// same rejected identity each time. `creds::callbacks` self-limits at
    /// MAX_CRED_ATTEMPTS, and this is the only honest test of that cap: it
    /// needs a server that actually rejects. No env vars — the URL is a
    /// repository that does not exist, which GitHub answers with a credential
    /// challenge rather than a 404.
    #[test]
    #[ignore = "requires network; asserts a wrong token fails fast instead of looping 15×"]
    fn wrong_token_fails_once() {
        let bogus = "this-is-not-a-valid-token";
        let tmp = tempfile::tempdir().unwrap();
        let h = harness(tmp.path());
        let cred = CredentialSpec::Token {
            username: "x-access-token".to_string(),
            token: Secret::new(bogus.to_string()),
            bound_host: None,
        };
        h.svc
            .put_repo(
                "nope",
                def(
                    "nope",
                    "https://github.com/rust-lang/does-not-exist-9f3a2b41.git",
                    Some(cred),
                ),
            )
            .expect("define repo");

        let started = std::time::Instant::now();
        let view = run_job(&h, "nope", JobOp::Clone);
        let elapsed = started.elapsed();

        assert_eq!(
            view.state,
            JobState::Failed,
            "a bogus token was accepted for a nonexistent repository"
        );
        assert_eq!(code_of(&view), "auth_failed", "{:?}", message_of(&view));
        assert!(
            elapsed < std::time::Duration::from_secs(30),
            "took {elapsed:?} — the credential retry cap is not holding"
        );
        let body = serde_json::to_string(&view).unwrap();
        assert!(!body.contains(bogus), "the error leaked the token");
    }
```

- [ ] **Step 33: Compile it, then run it**

Run:

```bash
cargo test --bin chrome-host-app -- --ignored --list
cargo test --bin chrome-host-app git::tests::wrong_token_fails_once \
  -- --ignored --exact --nocapture
```

Expected: the list now shows five ignored tests, then
`test git::tests::wrong_token_fails_once ... ok` in well under 30 seconds
(typically 2–5). A failure reporting
`took 45s — the credential retry cap is not holding` means `MAX_CRED_ATTEMPTS`
is not being enforced and libgit2 is looping. A failure reporting a code other
than `auth_failed` means `OpCtx::record_cred_failure` is not being consulted by
`classify_err`, and the callback error is falling through to the generic
`(GenericError, Callback)` arm.

- [ ] **Step 34: Commit**

```bash
git add src/git/mod.rs
git commit -m "test(git): add the credential retry-cap test

libgit2 replays a rejected credential up to 15 times; creds::callbacks caps
it at MAX_CRED_ATTEMPTS and reports auth_failed. Only a server that really
rejects can prove the cap holds, so this asserts both the code and a
sub-30-second wall clock. Needs no credentials — a nonexistent GitHub repo
answers a bad token with a challenge."
```

---

## Part E — final gate

- [ ] **Step 35: Run the full gate**

Run:

```bash
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo test --bin chrome-host-app -- --ignored --list
```

Expected: `cargo fmt --check` prints nothing and exits 0; clippy prints
`Finished` with no warnings; `cargo test --locked` ends
`test result: ok. N passed; 0 failed; 5 ignored`; and the last command lists
exactly five entries —

```
chrome::tests::real_chrome_reports_version: test
git::tests::real_https_clone_and_push_and_server_rejection: test
git::tests::real_ssh_clone_pins_the_host_key: test
git::tests::vendored_tls_trusts_public_ca: test
git::tests::wrong_token_fails_once: test
```

(order may differ). If `cargo fmt --check` reports a diff, run `cargo fmt` and
amend the last commit rather than adding a formatting-only commit.

- [ ] **Step 36: Final documentation audit and a clean tree**

Run:

```bash
sed -n '/^## 9\. Git service/,$p' README.md | grep -n 'TODO\|TBD\|FIXME\|XXX'
grep -n 'GIT_TEST_HTTPS_URL\|GIT_TEST_TOKEN\|GIT_TEST_SSH_URL\|GIT_TEST_SSH_KEY' README.md
grep -c '^## ' README.md
git status --short
```

Expected: the first grep produces **no output** (exit 1 — §9 has no placeholders
left); the second shows all four test env vars documented in §9.7 (five hits:
the four assignments in the run block, plus the `GIT_TEST_SSH_KEY` sentence
below it); the third prints `9` (§1 through §9); and `git status --short` prints
nothing — every change from this task is committed.

Scoped to §9 like step 8's, and for the same reason: §7's "code signing and
notarization … are a per-app TODO" is deliberate prose, so grepping the whole
file could never pass.
