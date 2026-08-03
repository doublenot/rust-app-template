### Task 0: Split main.rs into main.rs + host.rs

`src/main.rs` is 731 lines today and the git service adds ~110 more. This task lands the
split as its own **pure move, zero behaviour change** commit, so that the git PR's diff can
show at a glance that `kill_children`, `restart`, and the generation counters were never
touched. Nothing here is new code except one 8-line dialog helper.

The cut is already latent in the file: everything above `fn main()` is the host state
machine, everything from `fn main()` down is bootstrap + event dispatch.

**Files:**
- Create: `src/host.rs` — 446 lines, of which 436 are moved verbatim from `main.rs` and
  10 are a new `use` header.
- Modify: `src/main.rs:31-58` (delete — `struct Children`, `struct App`), `src/main.rs:89-495`
  (delete — the whole `impl App` block), `src/main.rs:3-21` (module + import header),
  `src/main.rs:24` and `src/main.rs:60` and `src/main.rs:69` (visibility), and one insertion
  before `src/main.rs:69`. Ends at 309 lines.

**Interfaces:**

- Consumes: nothing. This is the first task in the plan; the repo is at `main`, clean,
  `cargo test` green (32 passed, 1 ignored).

- Produces — the module `crate::host` and these items, which tasks 9, 10 and 11 edit or call:

```rust
// src/main.rs (crate root)
pub(crate) enum UserEvent {
    Menu(MenuId),
    Host(HostEvent),
    ChromeExited { generation: u64 },
    ServerExited { generation: u64 },
}
pub(crate) fn dialog(title: &str, description: &str);              // rfd MessageLevel::Error
pub(crate) fn notice(title: &str, description: &str);              // rfd MessageLevel::Warning
pub(crate) fn missing_chrome_dialog(app_name: &str);

// src/host.rs
pub(crate) struct Children {
    pub(crate) chrome: Option<tokio::process::Child>,
    pub(crate) server: Option<supervisor::ServerHandle>,
}
pub(crate) struct App {
    pub(crate) cfg: AppConfig,
    pub(crate) paths: RuntimePaths,
    pub(crate) chrome_exe: PathBuf,
    pub(crate) port: u16,
    pub(crate) status: Arc<RwLock<AppStatus>>,           // tokio::sync::RwLock
    pub(crate) children: Arc<Mutex<Children>>,           // tokio::sync::Mutex
    pub(crate) chrome_generation: Arc<AtomicU64>,
    pub(crate) server_generation: Arc<AtomicU64>,
    pub(crate) host_log: std::fs::File,
    pub(crate) rt: tokio::runtime::Handle,
    pub(crate) proxy: tao::event_loop::EventLoopProxy<UserEvent>,
    pub(crate) in_tray_mode: bool,
}
impl App {
    pub(crate) fn log(&mut self, msg: &str);
    pub(crate) fn chrome_generation(&self) -> u64;
    pub(crate) fn server_generation(&self) -> u64;
    pub(crate) fn refresh_tray(&mut self, tray_handle: &mut Option<tray::Tray>, show_open: bool);
    pub(crate) fn start_children(&mut self);
    pub(crate) fn restart(&mut self);
    pub(crate) fn quit(&mut self) -> !;
    pub(crate) fn restart_chrome_only(&mut self);
    pub(crate) fn open_settings(&mut self);
    // private to src/host.rs — no caller outside the file, so they stay private:
    // chrome_log, target_url, initial_chrome_url, spawn_chrome_watcher,
    // spawn_server_watcher, kill_children
}
```

- Produces — line anchors later tasks are pinned to. Original `main.rs` line `N` in
  `89..=495` becomes `src/host.rs` line `N - 49`; original line `N` in `31..=59` becomes
  line `N - 20`. Concretely, after this task:

| item | `src/host.rs` line |
|---|---|
| `pub(crate) struct Children {` | 11 |
| `pub(crate) struct App {` | 16 |
| `impl App {` | 40 |
| `fn refresh_tray` | 90 |
| `fn start_children` | 192 |
| the `supervisor::build_env(...)` call (was `main.rs:256`) | 207 |
| `fn kill_children` | 305 |
| `fn quit` | 328 |
| closing `}` of `impl App` | 446 |

  and in `src/main.rs`: `fn dialog` at 31, `fn notice` at 46, `fn missing_chrome_dialog`
  at 55, `fn main()` at 75, `fn run(` at 163, `event_loop.run(` at 211, EOF at 309.

---

- [ ] **Step 1: Record the baseline this task must not change**

There is no new behaviour to test, so the "test" is the *inventory* of the existing suite
plus the summary line. Capture both before touching anything. Run from the repo root:

```bash
cargo test 2>&1 | grep -E '^test .* \.\.\. ' | sed 's/ \.\.\..*//' | sort > /tmp/task0-tests-before.txt
wc -l < /tmp/task0-tests-before.txt
wc -l < src/main.rs
```

Expected, exactly:

```
33
731
```

`33` is 32 passing plus `chrome::tests::real_chrome_reports_version`, which is `#[ignore]`d.
**If the first number is `0`, stop** — your shell is filtering `cargo`'s output (some
setups wrap `cargo` in a summarising proxy) and every later comparison in this task would
pass vacuously. Re-run the capture with the real binary, e.g. `command cargo test …`, and
only continue once it prints `33`.

Also record the summary line for step 8:

```bash
cargo test 2>&1 | grep -E '^test result' > /tmp/task0-summary-before.txt
cat /tmp/task0-summary-before.txt
```

Expected (the two suites are the `chrome-host-app` binary and the `gen_icons` helper binary;
the `finished in` timings will differ run to run and are not part of the comparison):

```
test result: ok. 32 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 1.52s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

---

- [ ] **Step 2: Cut the host state machine out of main.rs into src/host.rs**

Two `sed` extractions, done mechanically so the moved text is provably byte-identical.
Run both from the repo root, **in this order** (the second rewrites `main.rs`, so the first
must read it while it is still whole):

```bash
cat > src/host.rs <<'EOF'
use crate::config::{AppConfig, OnClose, RuntimePaths};
use crate::internal_server::AppStatus;
use crate::{chrome, dialog, settings, supervisor, tray, UserEvent};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};

EOF
sed -n '31,59p;89,495p' src/main.rs >> src/host.rs
sed -n '1,30p;60,88p;497,731p' src/main.rs > /tmp/task0-main.rs.new
mv /tmp/task0-main.rs.new src/main.rs
wc -l src/main.rs src/host.rs
```

Expected:

```
  294 src/main.rs
  446 src/host.rs
  740 total
```

Why those exact ranges: `31-58` is `struct Children` + `struct App`, `59` is the blank line
that separated them from `fn dialog`, `89-495` is the whole `impl App` block. `1-30` is the
crate header through `enum UserEvent`, `60-88` is `dialog` + `missing_chrome_dialog`,
`497-731` is `main()` + `run()` + the `event_loop.run` dispatch match.

Why that `use` header: the moved bodies say `supervisor::open_log`, `chrome::launch`,
`tray::menu_model`, `settings::env_vars` and `dialog(...)` with no leading `crate::`.
Importing the sibling modules by name keeps every moved line byte-for-byte unchanged instead
of rewriting ~30 call sites — which is what makes the "pure move" claim checkable in step 7.

---

- [ ] **Step 3: Wire the new module into main.rs**

Five edits to `src/main.rs`, all in the first 21 lines.

Add the module declaration (the list is alphabetical):

```rust
mod chrome;
mod config;
mod host;
mod internal_server;
```

Import the two moved types — `run()` still constructs them:

```rust
use config::{AppConfig, OnClose, RuntimePaths};
use host::{App, Children};
use internal_server::{AppStatus, HostEvent, HostState};
```

Delete `use std::io::Write;` (only `App::log` used it) and `use std::time::Duration;` (only
the watchers and `start_children` used it), and drop `Ordering` from the atomic import —
`main.rs` now reaches the counters through `App::chrome_generation()` / `App::server_generation()`
and only names `AtomicU64` in the `App` literal:

```rust
use std::sync::atomic::AtomicU64;
```

The `use` block must end up exactly this, and nothing else in the file changes in this step:

```rust
use config::{AppConfig, OnClose, RuntimePaths};
use host::{App, Children};
use internal_server::{AppStatus, HostEvent, HostState};
use rand::Rng;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tokio::sync::{mpsc, Mutex, RwLock};
use tray_icon::menu::{MenuEvent, MenuId};
```

---

- [ ] **Step 4: Build and watch it fail on visibility**

Run: `cargo build`

Expected: FAIL with exactly two errors —

```
error[E0603]: struct `App` is private
  --> src/main.rs:12:12
   |
12 | use host::{App, Children};
   |            ^^^ private struct
   |
note: the struct `App` is defined here
  --> src/host.rs:16:1

error[E0603]: struct `Children` is private
  --> src/main.rs:12:17

error: could not compile `chrome-host-app` (bin "chrome-host-app") due to 2 previous errors
```

Note what is *not* here: `dialog` and `UserEvent` stayed private and `src/host.rs` imports
them without complaint. A child module can read its ancestors' private items — privacy in
Rust is "visible to the defining module and its descendants". The reverse is not true, which
is why `main.rs` cannot see into `host`.

---

- [ ] **Step 5: Make the two moved types crate-visible, and watch the failure change**

In `src/host.rs`:

```rust
pub(crate) struct Children {
```

```rust
pub(crate) struct App {
```

Run: `cargo build`

Expected: FAIL with 35 errors. Resolution now succeeds, so type-checking runs and reports
every field and method `main.rs` reaches through:

```
error[E0616]: field `cfg` of struct `App` is private          (×2)
error[E0616]: field `in_tray_mode` of struct `App` is private (×9)
error[E0616]: field `paths` of struct `App` is private        (×1)
error[E0616]: field `rt` of struct `App` is private           (×1)
error[E0616]: field `status` of struct `App` is private       (×1)
error[E0624]: method `log` is private                         (×3)
error[E0624]: method `chrome_generation` is private           (×1)
error[E0624]: method `server_generation` is private           (×1)
error[E0624]: method `refresh_tray` is private                (×7)
error[E0624]: method `start_children` is private              (×1)
error[E0624]: method `restart` is private                     (×3)
error[E0624]: method `quit` is private                        (×3)
error[E0624]: method `restart_chrome_only` is private         (×1)
error[E0624]: method `open_settings` is private               (×1)
error: could not compile `chrome-host-app` (bin "chrome-host-app") due to 35 previous errors
```

---

- [ ] **Step 6: Make the fields and the nine cross-module methods crate-visible**

The error list above is not the whole set — `run()` also *constructs* `App` and `Children`,
so every field needs `pub(crate)`, not just the five that are read. Mark all of them.

`src/host.rs` lines 11-38 become exactly (the doc comments are moved text — leave them alone):

```rust
pub(crate) struct Children {
    pub(crate) chrome: Option<tokio::process::Child>,
    pub(crate) server: Option<supervisor::ServerHandle>,
}

pub(crate) struct App {
    pub(crate) cfg: AppConfig,
    pub(crate) paths: RuntimePaths,
    pub(crate) chrome_exe: PathBuf,
    pub(crate) port: u16,
    pub(crate) status: Arc<RwLock<AppStatus>>,
    pub(crate) children: Arc<Mutex<Children>>,
    /// Bumped whenever the Chrome child is killed on purpose, so its watcher
    /// (and any exit event already in flight) is recognized as intentional.
    /// Kept separate from `server_generation` because tray "Open" replaces
    /// only the Chrome child: bumping a shared counter there would also
    /// retire the still-valid server watcher and make the in-flight
    /// `wait_healthy` task discard its result, stranding the status at
    /// `Starting`.
    pub(crate) chrome_generation: Arc<AtomicU64>,
    /// Bumped whenever the server child is killed on purpose. Guards the
    /// server watcher and the `wait_healthy` status write.
    pub(crate) server_generation: Arc<AtomicU64>,
    pub(crate) host_log: std::fs::File,
    pub(crate) rt: tokio::runtime::Handle,
    pub(crate) proxy: tao::event_loop::EventLoopProxy<UserEvent>,
    pub(crate) in_tray_mode: bool,
}
```

Then prefix these nine `impl App` methods — and only these nine — with `pub(crate) `:

| line | before | after |
|---|---|---|
| 41 | `    fn log(&mut self, msg: &str) {` | `    pub(crate) fn log(&mut self, msg: &str) {` |
| 76 | `    fn chrome_generation(&self) -> u64 {` | `    pub(crate) fn chrome_generation(&self) -> u64 {` |
| 80 | `    fn server_generation(&self) -> u64 {` | `    pub(crate) fn server_generation(&self) -> u64 {` |
| 90 | `    fn refresh_tray(&mut self, tray_handle: &mut Option<tray::Tray>, show_open: bool) {` | `    pub(crate) fn refresh_tray(&mut self, tray_handle: &mut Option<tray::Tray>, show_open: bool) {` |
| 192 | `    fn start_children(&mut self) {` | `    pub(crate) fn start_children(&mut self) {` |
| 321 | `    fn restart(&mut self) {` | `    pub(crate) fn restart(&mut self) {` |
| 328 | `    fn quit(&mut self) -> ! {` | `    pub(crate) fn quit(&mut self) -> ! {` |
| 352 | `    fn restart_chrome_only(&mut self) {` | `    pub(crate) fn restart_chrome_only(&mut self) {` |
| 406 | `    fn open_settings(&mut self) {` | `    pub(crate) fn open_settings(&mut self) {` |

Leave `chrome_log`, `target_url`, `initial_chrome_url`, `spawn_chrome_watcher`,
`spawn_server_watcher` and `kill_children` private — they have no caller outside
`src/host.rs`, and `pub(crate)` on an unused item is a `dead_code` error under
`-D warnings`.

Finally, the contract also asks for `pub(crate)` on the two crate-root items `src/host.rs`
names, plus `missing_chrome_dialog`. These are no-ops for the compiler (a private root item
is already crate-visible) but they document which side of the cut each item serves. In
`src/main.rs`:

```rust
pub(crate) enum UserEvent {
```

```rust
pub(crate) fn dialog(title: &str, description: &str) {
```

```rust
pub(crate) fn missing_chrome_dialog(app_name: &str) {
```

Run: `cargo build`

Expected: PASS —

```
   Compiling chrome-host-app v0.1.0 (/home/doublenot/sites/doublenot/rust-app-template)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 8.07s
```

---

- [ ] **Step 7: Prove the move was verbatim**

This is the point of the task. Two checks; both must pass before you commit.

First — `src/host.rs` below its 10-line header is character-identical to the lines it came
from, once the `pub(crate) ` markers are stripped back off:

```bash
git show HEAD:src/main.rs | sed -n '31,59p;89,495p' > /tmp/task0-a.txt
tail -n +11 src/host.rs | sed 's/^pub(crate) //; s/^    pub(crate) /    /' > /tmp/task0-b.txt
diff /tmp/task0-a.txt /tmp/task0-b.txt && echo VERBATIM_OK
```

Expected: no diff output, then `VERBATIM_OK`.

Second — `src/main.rs` gained nothing but the header rewiring and two visibility keywords:

```bash
git diff -U0 -- src/main.rs | grep '^+[^+]'
```

Expected: exactly these five added/changed lines and nothing else (every other line of the
diff is a `-`):

```
+mod host;
+use host::{App, Children};
+use std::sync::atomic::AtomicU64;
+pub(crate) enum UserEvent {
+pub(crate) fn dialog(title: &str, description: &str) {
+pub(crate) fn missing_chrome_dialog(app_name: &str) {
```

(`git` renders the `pub(crate) fn dialog` change as a rewritten hunk, so the six unchanged
body lines of `dialog` may also appear as `+` lines; `dialog`'s body text itself is
untouched. Nothing else may appear.)

And the shape of the whole change:

```bash
git add -A && git diff --cached --stat
```

Expected:

```
 src/host.rs | 446 ++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
 src/main.rs | 460 +++---------------------------------------------------------
 2 files changed, 465 insertions(+), 441 deletions(-)
```

441 deletions from `main.rs`, 446 insertions in `host.rs` — the 436 moved lines plus the
10-line header, and 19 changed/added lines in `main.rs`.

---

- [ ] **Step 8: Prove behaviour is unchanged, then commit the pure move**

```bash
cargo fmt --check && echo FMT_CLEAN
cargo clippy --all-targets -- -D warnings
cargo test 2>&1 | grep -E '^test .* \.\.\. ' | sed 's/ \.\.\..*//' | sort > /tmp/task0-tests-after.txt
diff /tmp/task0-tests-before.txt /tmp/task0-tests-after.txt && echo SAME_TESTS
cargo test 2>&1 | grep -E '^test result'
wc -l src/main.rs src/host.rs
```

Expected:

```
FMT_CLEAN
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.74s
SAME_TESTS
test result: ok. 32 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 1.53s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
  294 src/main.rs
  446 src/host.rs
  740 total
```

`SAME_TESTS` is the real assertion: the same 33 test names in the same modules, because
`main.rs` had no `#[cfg(test)] mod tests` and no test moved. The two `test result` lines
must match `/tmp/task0-summary-before.txt` apart from the timings.

```bash
git add src/main.rs src/host.rs
git commit -m "refactor: move the App state machine out of main.rs into host.rs

Pure move, no behaviour change: struct App, struct Children and every
impl App method go to src/host.rs; main.rs keeps UserEvent, the dialogs,
main(), run() and the event_loop.run dispatch match. App's fields and the
nine methods run() calls become pub(crate) so the crate root can still
reach them.

main.rs 731 -> 294 lines. Landed on its own so the git-service diff that
follows shows kill_children, restart and the generation counters
untouched."
```

---

- [ ] **Step 9: Add `notice()` and watch clippy reject it**

The git service needs a non-error modal (merge-conflict-resolved notifications, deferred
restarts) that is not the red `dialog`. It belongs beside `dialog` in `main.rs`, and adding
it now keeps `main.rs`'s entire diff for this feature in one commit.

Insert into `src/main.rs` between `dialog`'s closing `}` (line 38) and
`pub(crate) fn missing_chrome_dialog` (now line 40):

```rust
/// Non-error modal: same shape as `dialog` at warning level, for outcomes the
/// user should see that did not stop the app from working.
pub(crate) fn notice(title: &str, description: &str) {
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Warning)
        .set_title(title)
        .set_description(description)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}
```

Run: `cargo clippy --all-targets -- -D warnings`

Expected: FAIL — nothing calls it yet, and this is a binary crate, so `dead_code` fires:

```
error: function `notice` is never used
  --> src/main.rs:40:15
   |
40 | pub(crate) fn notice(title: &str, description: &str) {
   |               ^^^^^^
   |
   = note: `-D dead-code` implied by `-D warnings`
   = help: to override `-D warnings` add `#[expect(dead_code)]` or `#[allow(dead_code)]`

error: could not compile `chrome-host-app` (bin "chrome-host-app") due to 1 previous error
```

---

- [ ] **Step 10: Silence it with `#[expect]`, not `#[allow]`, and commit**

Replace the doc comment and add the attribute, so `src/main.rs` lines 40-53 read exactly:

```rust
/// Non-error modal: same shape as `dialog` at warning level, for outcomes the
/// user should see that did not stop the app from working.
///
/// `expect` rather than `allow` so the attribute cannot outlive its reason —
/// the first caller turns the unfulfilled expectation back into a warning.
#[expect(dead_code)]
pub(crate) fn notice(title: &str, description: &str) {
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Warning)
        .set_title(title)
        .set_description(description)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}
```

`#[expect]` is stable since Rust 1.81 and this crate's `rust-version` is 1.85. Unlike
`#[allow]`, an `#[expect]` whose lint *stops* firing becomes an `unfulfilled_lint_expectation`
warning — which is an error under `-D warnings`. So the task that adds the first `notice(...)`
call site is forced to delete the attribute; it cannot silently rot.

Run:

```bash
cargo fmt --check && echo FMT_CLEAN
cargo clippy --all-targets -- -D warnings
cargo test 2>&1 | grep -E '^test result'
wc -l src/main.rs src/host.rs
```

Expected: PASS —

```
FMT_CLEAN
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.70s
test result: ok. 32 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 1.52s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
  309 src/main.rs
  446 src/host.rs
  755 total
```

```bash
git add src/main.rs
git commit -m "feat(host): add notice(), the warning-level sibling of dialog()

Same rfd shape as dialog() at MessageLevel::Warning, for outcomes the
user should see that did not stop the app from working. No caller yet;
#[expect(dead_code)] rather than #[allow] so the first caller has to
remove the attribute."
```

---

**Definition of done:** `cargo test` (32 passed, 1 ignored, same 33 names as before),
`cargo fmt --check`, and `cargo clippy --all-targets -- -D warnings` all pass;
`git diff --stat` against the task's parent commit shows `src/host.rs` created and
`src/main.rs` reduced, with no line of moved code altered.

**One deviation to flag to the plan reviewer.** The spec's §2.1 estimate ("`main.rs` ~260 LOC")
and slice-0 gate ("≤ 270 LOC") are not reachable while `main.rs` keeps everything the
interface contract assigns to it. The arithmetic: crate header + `UserEvent` (30) + `dialog`
+ `notice` + `missing_chrome_dialog` (44) + `main()` (86) + `run()` and the `event_loop.run`
match (147) = **309**, with nothing left to remove that the contract does not name as
`main.rs`'s. Getting under 280 would mean moving `App`'s construction or the dispatch arms
into `host.rs`, which is a restructure, not a move, and would invalidate task 10's
`run()`-signature change (contract §6.7) and its "3 event arms" edit. 309 is the floor for a
pure move; the gate should read ≤ 310.
