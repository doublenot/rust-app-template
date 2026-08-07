# Hitch

**Ride the Chrome you already have.** A native Rust shell for your web app —
no bundled browser engine, no embedded webview.

## 1. What this is

This is a template for shipping a desktop application as a small Rust host
process that opens **Google Chrome in app mode** (`--app=<url>`), pointed at
either a URL of your choosing or a local server that the host itself starts
and supervises. The Rust binary owns the window lifecycle, an optional
companion process (e.g. a Node/Python/whatever server), a settings screen,
and a system tray icon — Chrome only ever renders the page. There is no
embedded webview and no bundled browser engine: it drives the Chrome already
installed on the machine.

On a fresh clone, `app.toml`'s `[app].url` is empty, so the host serves a
built-in placeholder page (from its internal loopback server) explaining
that no URL is configured yet, and opens Chrome against that. Point `url` at
a real address, or configure `[server]` to launch a local process, to make
it do something useful.

## 2. Quick start

Prerequisites:

- Rust stable, edition 2021, **rust-version 1.85 or newer**.
- **Google Chrome** installed and on the machine at runtime (Chromium, Edge,
  and other Chrome-based browsers are not supported or detected). Get it
  from <https://www.google.com/chrome/>.
- On Linux, build-time system packages for the tray icon, dialogs, and
  `xdg`/X11 helpers:

  ```bash
  sudo apt-get install libgtk-3-dev libayatana-appindicator3-dev libxdo-dev \
                       perl make pkg-config
  ```

  `perl`, `make` and `pkg-config` are for the vendored OpenSSL that the git
  service's dependencies build from source — see §7 for why, and for how to
  opt out.

  and, at runtime (on a machine that only has the built binary, not a full
  dev toolchain):

  ```bash
  sudo apt-get install libayatana-appindicator3-1
  ```

Then:

```bash
cargo run
```

This builds the host, validates the embedded `app.toml`, starts the
internal loopback server, optionally launches and health-checks `[server]`,
and opens Chrome in app mode against the configured (or placeholder) page.

## 3. Configuration reference

All configuration lives in `app.toml` at the repo root and is embedded into
the binary at compile time via `include_str!` — editing it requires a
rebuild (`cargo build` / `cargo run`), there is no runtime config reload.
Unknown keys anywhere in the file are rejected (`deny_unknown_fields`), so
typos fail loudly instead of being silently ignored.

### `[app]` (required)

| key | type | default | behavior |
|---|---|---|---|
| `name` | string | — (required, non-empty) | Window title and tray tooltip. |
| `identifier` | string | — (required) | Reverse-domain identifier, e.g. `com.example.myapp`. Must have at least two dot-separated parts, each non-empty and containing only ASCII alphanumerics and `-`. Names the app's data directory under the OS data dir (see §6). |
| `url` | string | `""` | Target URL Chrome is pointed at. Empty string means "no URL configured" — the host serves and opens its own built-in placeholder page instead of failing. |

### `[server]` (optional; entire section omitted by default)

Present only if you want the host to launch and supervise a local
companion process before showing the app.

| key | type | default | behavior |
|---|---|---|---|
| `command` | array of strings | — (required, non-empty) | argv for the child process; run directly, no shell involved. `command[0]` is the executable, the rest are its arguments. |
| `cwd` | string, optional | host's base dir | Working directory for the child. A relative path is joined onto the host's base directory (see §4 for how that base directory is chosen); an absolute path is used as-is. |
| `env` | table of string→string | `{}` | Extra environment variables merged into the child's environment (see §4 for the full env contract). |
| `health_check_url` | string | — (required, non-empty) | URL the host polls (every 500ms) until it returns a status code below 400, or `startup_timeout_secs` elapses. |
| `startup_timeout_secs` | integer | `60` | How long to wait for the health check to succeed before treating startup as failed. |

### `[window]`

| key | type | default | behavior |
|---|---|---|---|
| `width` | integer | `1200` | Initial Chrome app-mode window width, in pixels. |
| `height` | integer | `800` | Initial Chrome app-mode window height, in pixels. |
| `on_close` | string: `"quit"` \| `"tray"` | `"quit"` | What happens when the main app window is closed. `"quit"` exits the whole host (and any supervised server). `"tray"` keeps the host and tray icon running in the background; the app can be reopened from the tray menu without paying the `[server]` startup cost again. Any other string is rejected at load time. |

Window close is observed indirectly, as the exit of the Chrome *process* the
host owns — so under `on_close = "quit"` the host quits only when the **last**
window of that Chrome instance closes: if a Settings window (or any other
extra window on the same profile) is still open, closing the app window alone
does not exit the app.

### `[menu]`

| key | type | default | behavior |
|---|---|---|---|
| `settings` | bool | `false` | Whether the tray menu shows a "Settings…" entry that opens the settings page. Setting this `true` requires at least one `[[settings.fields]]` entry to be present — the host refuses to start otherwise. |
| `items` | array of tables | `[]` | Extra static tray menu entries. Each item is `{ id = "...", label = "...", open_url = "..." }`: `id` is a config-schema identifier (not read at runtime; reserved for future menu-editing tooling), `label` is the menu text, and `open_url` is opened in the user's default browser (not the app's Chrome instance) when clicked. |

### `[[settings.fields]]` (optional, repeatable)

Only meaningful when `menu.settings = true`. Each entry describes one field
on the settings page.

| key | type | behavior |
|---|---|---|
| `key` | string | Field identifier. Must match `[A-Za-z0-9_]+` and be unique across all fields *ignoring case* (`theme` and `Theme` collide, because both become `APP_SETTING_THEME`) — anything else fails validation. Also used to build the field's environment-variable name (see §5). |
| `label` | string | Human-readable label shown on the settings page. |
| `type` | string: `"text"` \| `"boolean"` \| `"select"` | Controls both the rendered input and the constraints on `default`/`options` below. |
| `default` | string, bool, or (for `select`) string | Default value used until the user saves a settings change. Type must match `type`: `text` requires a string default, `boolean` requires a `true`/`false` default, `select` requires a string default. |
| `options` | array of strings | Required and non-empty for `type = "select"` (and `default` must be one of them); must be omitted/empty for `text` and `boolean` — supplying it is a validation error. |

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
| `registry_writes` | bool | `true` | `false` makes `<data-dir>/repos.json` author-owned: `PUT` and `DELETE /api/git/repos/{id}` answer `403 registry_read_only`, and the app can only sync the repos you shipped. Not the only thing that closes writes — a `repos.json` the host could neither read nor quarantine does too (§9.7) — so a client must not read `registry_writable: false` in `GET /api/git` as "the author set this key". The fault case is the one that also carries a `registry_error`. |
| `default_branch` | string | `"main"` | Branch used by repo definitions that do not name one. Must be a valid branch name: non-empty, ≤ 200 bytes, only `[A-Za-z0-9._/-]`, no `..`, no `//`, no leading `-` or `/`, no trailing `/`, not ending in `.lock`, not exactly `@`, no ASCII control characters. |
| `author_name` | string | `""` | Commit author name. Empty means `[app].name`. Must not contain `<`, `>` or newlines — a git signature cannot represent them. |
| `author_email` | string | `""` | Commit author email. Empty means `<identifier>@<hostname>`. If set, must contain `@` and no `<`, `>` or whitespace. |
| `network_timeout_secs` | integer | `120` | libgit2's per-connection idle timeout and the deadline every fetch/push job runs against. Must be between 5 and 3600. |
| `quit_sync_timeout_secs` | integer | `10` | Upper bound on how long Quit waits for repos with `sync_on_quit`. `0` disables `sync_on_quit` entirely. Must be 120 or less. |
| `allow_http` | bool | `false` | Permit plaintext `http://` remotes. Left `false`, an `http://` remote is rejected with `insecure_remote`. |
| `ssh_host_key_policy` | string: `"tofu"` \| `"accept"` | `"tofu"` | `"tofu"` pins a remote's SSH host key the first time it is seen (into `git-state.json`) and fails `host_key_mismatch` if it ever changes. `"accept"` takes any host key — only for a network you control. |

These are validated at load time whether or not `[git]` is ever used, so
enabling a repo never surfaces a new config error at an awkward moment.

## 4. Local server contract

If `[server]` is configured, the host:

1. Resolves the working directory: `resolve_cwd(cwd, base_dir)`. `base_dir`
   is the repo root in debug builds (`CARGO_MANIFEST_DIR`, baked in at
   compile time) and the directory containing the running executable in
   release builds. `cwd` from config, if relative, is joined onto that
   base; if absolute, it's used verbatim; if omitted, `base_dir` itself is
   used.
2. Builds the child's environment by layering, in order: `[server].env`
   from `app.toml`, then one `APP_SETTING_<KEY>` variable per settings
   field (uppercased key, current value; see §5), then
   `APP_SETTINGS_FILE=<path to settings.json>` (always present, even
   when no settings schema is configured), then the host-access variables
   described below.
3. Spawns `command` with that cwd and environment, redirecting both stdout
   and stderr to `<data-dir>/logs/server.log` (append mode; see §6 for
   `<data-dir>`). The log file is size-capped: if it exceeds 5 MB when
   opened, the existing file is renamed to `server.log.old` (replacing any
   previous `.old`) and a fresh empty log is started.
4. Polls `health_check_url` every 500ms (2s per-request timeout) until it
   returns an HTTP status below 400, or `startup_timeout_secs` elapses
   (default 60s) — whichever comes first. Chrome opens immediately on the
   host's own `/loading` page; that page polls `/api/status` and only
   redirects to the target URL once the health check succeeds. If the
   timeout is hit, the loading page flips to an error view linking to
   `server.log`, with a Retry button; the server child is left running until
   you retry (which restarts it) or quit from the tray.

The host's own log (its own diagnostics, not the child server's) is
written to `<data-dir>/logs/host.log` under the same rotation rule.

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

## 5. Settings

When `menu.settings = true` and at least one `[[settings.fields]]` entry
exists, the tray's "Settings…" entry opens a small Chrome window rendering
a form generated from that schema: one input per field (a text box, a
checkbox, or a dropdown populated from `options`, depending on `type`).
Values are read from and written to `<data-dir>/settings.json` (a flat
JSON object of `key -> value`, created with each field's `default` the
first time it's needed).

Submitting the settings form does not update the running instance of a
supervised `[server]` process in place — it saves the new values to
`settings.json` and restarts the host's children so the next launch of
`[server]` (and the `APP_SETTING_*` variables it receives) picks them up.
Mutating settings endpoints on the internal server additionally require the
`x-host-token` header to match a random token generated fresh for each
launch. That stops other *web pages* the user has open from driving the
host: the token is only embedded in pages the host itself serves, and the
browser's same-origin policy keeps other origins from reading it. It is not
an isolation boundary against other local processes — anything running as
the same user can fetch the unauthenticated `/loading` or `/settings` page
and read the token out of it.

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
  Its `bound_host` is recorded when you save it, and a `PUT` that omits
  `credential` keeps the stored one **only while the remote still resolves to
  that same host** — the plain equality, with "no host" as a value like any
  other. An absent `bound_host` is not a wildcard: a credential saved next to a
  hostless remote (`file://`, a local path, or no remote at all) is bound to
  nothing, and it is kept only while that stays true. So all three of these
  **drop** it and return an `auth_unbound` warning: repointing the remote at a
  different host, removing the remote, and giving a hostless remote a real one.
  An operation that would need an unbound credential fails `auth_unbound`
  rather than transmitting it. That closes the worst primitive the feature
  would otherwise create: repoint a repo at an attacker's host, trigger a
  fetch, receive the user's PAT.
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

## 6. Data locations

Runtime state lives under the OS's standard per-user data directory, in a
subdirectory named after `[app].identifier`:

| OS | base data dir |
|---|---|
| Windows | `%APPDATA%` |
| macOS | `~/Library/Application Support` |
| Linux | `~/.local/share` |

Under `<base>/<identifier>/` (the `RuntimePaths` the host computes at
startup):

| path | contents |
|---|---|
| `chrome-profile/` | Chrome's dedicated user-data-dir for this app — kept separate from the user's normal Chrome profile. |
| `logs/` | `server.log` (+ `.old`) for the supervised `[server]` child, `host.log` (+ `.old`) for the host itself, `chrome.log` (+ `.old`) for Chrome's own stdout/stderr (GPU probes, service chatter — kept out of the host's terminal), and `git.log` (+ `.old`) for the git service (§9) when `[git]` is enabled. All rotate at 5 MB. |
| `settings.json` | Persisted settings values, keyed by settings field `key`. |
| `app.lock` | Single-instance lock file; prevents two copies of the app running against the same identifier at once. |
| `repos/` | One working tree per defined repo, at `repos/<id>/`. Each is an ordinary git repository you can `cd` into and run `git` in. Created by the git service on first use; never created when `[git]` is absent. |
| `repos.json` | Git repo definitions, **including credentials**. `0600` on unix. Written only by `PUT`/`DELETE /api/git/repos/{id}` — never by a sync, a timer or a job. Read once at startup; not hot-reloaded. Bytes the host **has** and cannot use — not UTF-8, not valid JSON, wrong shape — are renamed to `repos.json.corrupt-<epoch_ms>` and never deleted, and the registry stays writable. A file it could not **read** at all (permissions, I/O, a directory in its place) is deliberately left exactly where it is: renaming a file we never read is a bigger act than refusing to write one, so instead every registry write is refused until it can be read or moved aside (§9.7). |
| `git-state.json` | Host-owned volatile git state: each repo's last sync outcome and its pinned SSH host fingerprint. Rewritten after every finished job. Never contains a secret; safe to delete, at the cost of the last-sync record and the host-key pin. |

## 7. Building and packaging

### Building on each OS

The same commands build everywhere once the per-OS prerequisites are in
place:

```bash
cargo build --release   # optimized binary in target/release/
cargo run               # debug build + launch (development)
```

The release binary lands at `target/release/hitch`
(`hitch.exe` on Windows) and embeds `app.toml`, the internal
pages, and the icon — it is self-contained apart from Google Chrome itself
and, on Linux, the tray runtime library.

| OS | toolchain | extra build dependencies |
|---|---|---|
| Linux (Debian/Ubuntu) | Rust 1.85+ via [rustup](https://rustup.rs) | `sudo apt-get install libgtk-3-dev libayatana-appindicator3-dev libxdo-dev perl make pkg-config` (same list as §2) |
| macOS | Rust 1.85+ via rustup, plus Xcode Command Line Tools (`xcode-select --install`) | **`perl` and `make`** — both ship with the Command Line Tools, so in practice nothing extra to install |
| Windows | Rust 1.85+ via rustup with the default MSVC toolchain (requires [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) with "Desktop development with C++") | none — MSVC only. No perl, no make, no OpenSSL |

Google Chrome is a **runtime** requirement on every OS, never a build-time
one. Cross-compiling between OSes is not supported — build on each target
OS, or let CI do it: `.github/workflows/ci.yml` builds and tests all three
platforms on pull requests and on pushes to `main`, and the release workflow
packages installers for all three on every `v*` tag (see §8).

Note the trigger list: a branch pushed to the remote **without** a pull request
gets no CI at all, so the macOS and Windows legs first see the work only after
it has already landed on `main`. Open a pull request for anything non-trivial
even when you intend to fast-forward it.

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

### Installers

Build local installers with [`cargo-packager`](https://github.com/crabnebula-dev/cargo-packager):

```bash
cargo install cargo-packager
cargo packager --release
```

`cargo-packager` reads the `[package.metadata.packager]` table in
`Cargo.toml` (product name, identifier, icon list) and produces the
platform-native formats available on the host you build on — e.g. `.deb`/
AppImage on Linux, `.msi`/NSIS on Windows, `.app`/`.dmg` on macOS. It does
not cross-compile installers for other OSes; build on each target OS (or
use the release workflow's CI matrix) to get all of them.

Replace `icons/icon.png` (512×512, generated by `src/bin/gen_icons.rs` as a
placeholder — run `cargo run --bin gen_icons` only if you want to regenerate
that exact placeholder) with your app's real branding before shipping.

Code signing and notarization are **not configured** by this template and
are a per-app TODO:

- macOS notarization: <https://developer.apple.com/documentation/security/notarizing_macos_software_before_distribution>
- Windows code signing: <https://learn.microsoft.com/windows/msix/package/signing-package-overview>

The release workflow pins **cargo-packager 0.11.8** (`PACKAGER_VERSION` in
`.github/workflows/release.yml`). The installer filenames come from format
strings inside that tool and the workflow globs them, so pin the same version
locally if you want to reproduce what CI produces.

## 8. Releasing

`Cargo.toml` is the authority on the version. Cut a release with:

```bash
scripts/release.sh 1.2.0   # bumps [package].version, syncs Cargo.lock,
                           # commits, and creates the v1.2.0 tag
git push origin main
git push origin v1.2.0     # this is what starts the release
```

`scripts/release.sh` deliberately does not push. It refuses a dirty tree, a
version that is not semver, and a version that has already been tagged — any
given version can be tagged exactly once.

For a **first release**, where `Cargo.toml` already carries the version you want
to ship, pass it anyway:

```bash
scripts/release.sh 0.1.0   # nothing to bump; tags the current commit
git push origin v0.1.0
```

There is no version bump and no commit in that case, just the tag.

Pushing the tag triggers `.github/workflows/release.yml`, which:

1. **Checks the tag against the crate version**, failing in seconds with
   `::error::tag v1.2.0 != crate version 0.1.0` if they disagree. Without this a
   mismatched tag spends half an hour building installers named after the wrong
   version.
2. **Builds and packages on three runners** — `.deb` and AppImage on Linux, a
   universal `.dmg` on macOS, an NSIS `-setup.exe` on Windows.
3. **Creates one draft release** carrying all four installers plus
   `SHA256SUMS`. Every leg must succeed: if one fails, no release is created at
   all, so you never get a partial release that looks finished.

The release is a **draft**. Review it and publish it yourself — a broken build
never becomes public on its own.

Verify a download against the published checksums:

```bash
sha256sum -c SHA256SUMS --ignore-missing
```

(`--ignore-missing` lets you check a single installer without downloading all
four. Drop it to require every listed file to be present.)

### Dry runs

The workflow also accepts `workflow_dispatch`. Running it from the Actions tab
**against a branch** builds all three legs and uploads the installers as workflow
artifacts without touching a release — the way to check a packaging change before
committing to a tag. Running it **against a tag** does publish, which is how to
re-cut a release whose upload failed.

### macOS: universal, and ad-hoc signed

The `.dmg` contains a universal binary — `aarch64-apple-darwin` and
`x86_64-apple-darwin` both built on the arm64 runner and combined with `lipo` —
so one download serves Apple Silicon and Intel.

It is **ad-hoc signed** (`codesign --sign -`), and that is required rather than
cosmetic: `lipo` output carries only the minimal signature the Apple linker
applies, and recent macOS refuses to run linker-signed-only binaries on Apple
Silicon. Ad-hoc signing makes the app *run*; it does not make it *trusted*.
Gatekeeper still reports an unidentified developer, so the first launch needs
right-click → **Open**. Real signing and notarization remain a per-app TODO
(§7 above).

**[`docs/macos.md`](docs/macos.md)** covers macOS specifically: a checklist for
verifying a downloaded `.dmg`, how to build a universal one locally, and what to
do about the Gatekeeper warning.

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
name is refused outright (`merge_path_type_conflict`) rather than guessed at.

**Prefer-local only ever applies to files git is tracking.** A file it is not —
untracked, or covered by `.gitignore` — is never overwritten. If the remote
starts tracking a path such a file already occupies, the whole operation is
refused with `409 dirty_tree`, not retryable, naming every colliding path. That
is deliberately not the same rule as the merge above: an untracked file has no
second parent to recover it from, so there is no version of "keep the local
copy" that also lets the remote's copy land. It refuses even when the two files
are byte-identical — nothing has told git the local one is the same. That is
mostly a `pull` story, since `pull` never stages; a `sync` stages first, so its
untracked files become tracked ones and take the prefer-local path instead.
Ignored files are the case that survives staging, and so the case an unattended
timer actually hits.

Every refusal on this path leaves the repository byte-identical to where it
started — the working tree, the index and every ref, the `merge_path_type_conflict`
case included. The checkout runs *before* any ref moves and libgit2 counts the
collisions before it writes anything, so a refused sync is a perfect no-op that
the next sync simply redoes once the file is out of the way.

> **A repository this host finds mid-merge, mid-rebase or mid-cherry-pick was
> put there by a human.** Nothing in this service leaves one that way: its own
> merge is computed in memory and never writes `MERGE_HEAD`. So every mutating
> verb except `reset` refuses such a tree with `409 dirty_tree`, naming the
> state and handing back the exact reset body that discards it. That includes
> `push`, which real git allows mid-merge — a `push`-shaped hole in the rule is
> something every future reader would have to re-derive, and the cost is one
> `reset` or `git merge --abort` before publishing. The service never runs
> `cleanup_state()` or `reset --hard` on its own initiative: committing over an
> interrupted merge would stage the `<<<<<<<` markers as resolved content,
> record the branch being merged nowhere, delete the `MERGE_HEAD` that proves
> what was in flight, and then publish all of it.

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
  absent → clone, or `init` when there is no remote. No `remote` → stage and
  commit only. Otherwise: stage everything not ignored, commit if dirty, fetch,
  merge (§9.2), push. Send `push: false` to keep it local.
- **The three GET reads are synchronous, not jobs**, and they deliberately do
  *not* wait for a job running on the same repo — observing a repo while it
  syncs is the main reason you would call them. They are bounded at 2 seconds
  and answer `504 status_timeout` past that. A read taken mid-checkout is a
  snapshot; the job's `result` is authoritative.
- **`PUT` is whole-object create-or-replace.** There is no PATCH: one verb, no
  merge rules to document. A `PUT` on a repo with a job in flight is
  `409 repo_busy` with the running job in `error.job` — the job snapshotted its
  definition at admission, so a replace would silently not apply.
- **`PUT` and `DELETE` take the repo for the length of the call**, rather than
  checking that it looks idle and hoping. While one is in flight every other
  call for that id is refused with `repo_locked` — the other mutator and every
  job start, an `auto_sync_secs` tick included, which simply logs it and skips.
  Over HTTP that is a `409` with **no** `error.job`, because the holder is the
  host itself and there is nothing to poll. It is retryable, and the hold is
  released before the call that took it returns. This is what makes
  `?purge=true` safe: no libgit2 write can start against a tree while
  `remove_dir_all` is walking it. One honest wart: `status.busy_job` is a *job*
  reference, so it reads `null` while a hold is open — a caller polling through
  a slow purge sees "idle" and then gets `409 repo_locked` if it acts on it.
- **`branch` collapses create, checkout and set-upstream** into one call.
  `create: false`, `checkout: false` and no `upstream` is `422` ("nothing to
  do").
- **`reset` requires `confirm: true`.** Without it, `422 confirm_required`. It
  is also the only verb that will open a repository in a non-Clean state, and
  `{"to":"head","confirm":true}` is what discards an interrupted merge (§9.2) —
  worth knowing, because that is the exact body the refusal message hands you.
  The one gap: during a **rebase** HEAD is detached, so `reset` answers
  `detached_head` and the repair has to happen by hand in the tree.
- **There is no cancel endpoint and no file endpoint.** libgit2 is
  interruptible only inside its transfer callbacks, so a cancel would work
  during fetch and silently no-op during merge, checkout and TCP connect;
  it is replaced by `[git].network_timeout_secs` and a visible `stalled` flag
  on the job. And you already have the tree path — proxying file bytes through
  HTTP would double every byte and create a second, worse filesystem API.

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
| `remote` | `null` | `https://`, `ssh://`, `git://`, `file://`, `git@host:path`, or an absolute local path — absolute by the *host's* rule, so `C:\repos\x.git` or `\\server\share\x.git` on Windows and `/srv/repos/x.git` elsewhere; a leading `/` is only drive-relative on Windows and is refused there. `http://` is rejected as `insecure_remote` unless `[git].allow_http`; a URL carrying a password in its userinfo is always rejected, and on `http(s)` a *username* is rejected too — that is where a token is normally carried, and it would end up in this repo's `.git/config` in clear text. Put it on the repo as `credential` instead. `ssh://git@host` and `git@host:path` are unaffected. `null` means a local-only repo: `sync` inits and commits and reports `outcome: "committed"`, while `clone`/`pull`/`push` fail `remote_missing`. |
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

A `PUT` answers `201` the first time and `200` on every later replace, so a
`[server]` child can redefine its repos unconditionally on every boot. With
`[git].registry_writes = false` both `PUT` and `DELETE` answer
`403 registry_read_only` instead: `repos.json` is yours to write, and the app
can only sync the repos you shipped with it. **The same 403 has a second
producer**: a `repos.json` on disk that the host could neither read nor
quarantine closes writes too (§9.7). `GET /api/git` tells them apart without a
second error code — `registry_writable: false` **with** a `registry_error` is
the fault, without one it is the config key.

**Omitting `credential` on a replace does not erase it, and does not always
keep it.** The stored credential is carried forward — which is what keeps a PAT
off the loopback socket on every unrelated edit — but only while the remote in
*this* `PUT` still resolves to the host that credential is bound to. A `PUT`
that changes that host, that removes the remote, or that gives a previously
hostless remote (`file://`, a local path, `null`) a real one drops it, answers
`200` and returns an `auth_unbound` warning naming both sides. Send a
replacement credential in the same call when you intend that move; see §5 for
why an absent `bound_host` is not treated as a wildcard.

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

### 9.5 Jobs, retries, and errors

Every mutating call returns `202` immediately with a job body and a
`Location: /api/git/jobs/<job_id>` header. **One job per repo at a time**: a
second call while one is running is `409 repo_busy`, with `Retry-After: 1` and
the running job embedded in `error.job`. The same slot is what a `PUT` or
`DELETE` takes for the length of its call (§9.3), so a job start can also come
back `409 repo_locked` — same 409, no `error.job`, because the holder is the
host and not a job.

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

Retention never takes an id back from a live job: when the superseded record of
a failed attempt ages out of the fifty-job window, the successor admitted under
that same `request_id` keeps the entry, so the replay rules above still hold
however long the retry runs.

Because a matching `request_id` can never produce a 409, a 409 always means
someone else holds the repo — where "someone else" can be **the host itself**,
mid-`PUT` or mid-`DELETE`. Branch on `error.job`: present means a job you can
poll (`repo_busy`), absent means a host-side hold that will be gone in
milliseconds (`repo_locked`).

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
- **A `remote: null` repo is synced by all four of these.** It counts towards
  the "at least one repo" the tray entry needs, and **Sync now**, the
  `auto_sync_secs` timer, `sync_on_start` and `sync_on_quit` all really commit
  for it — staging and committing is the half of "sync" a local-only repo has
  (§9.4). There is nothing to fetch or push, so `fetched` and `pushed` stay
  `false` and the outcome is `committed` or `no_changes`.
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

  > **A settings change that arrives from the remote relaunches Chrome.** It
  > restarts the `[server]` child and reloads the window, which discards scroll
  > position, form state, and anything else the page was holding in memory —
  > the same cost as saving from the settings page, but triggered by someone
  > else's push. That is why the restart is gated on a *validated value*
  > actually differing rather than on the file changing: reformatting alone,
  > or a pull that brings back what you already had, restarts nothing. Leave
  > `sync_settings` off unless the settings really are shared state.

  > **The write-back races the settings page, and the loser's edit is lost.**
  > If someone saves from `/settings` in the same instant a sync adopts pulled
  > settings, whichever write lands second wins the whole file. It can never
  > corrupt it — the file is always written whole, so no reader sees a partial
  > document — and the worst case is one lost update, which the next save or
  > the next sync republishes. This is accepted rather than locked: the
  > collision needs a settings edit inside the same second as a sync, and the
  > obvious "fix" of skipping the write when the file moved underneath would
  > silently drop the *remote's* values instead, which is the worse loss.

### 9.7 Operational notes

- **You own `.gitignore`.** A sync stages everything the repo does not ignore.
  A `node_modules` inside a repo tree *will* be committed. Git's ignore rules
  are honoured and nothing is ever force-added, but the tree is yours by
  design. There is exactly one way an ignored file stops a sync: if the remote
  starts *tracking* a path this machine ignores, the sync refuses with
  `dirty_tree` rather than replacing your copy (§9.2), and on an
  `auto_sync_secs` timer the repo then sits in `failing` — one dialog, one log
  line per tick — until a human moves the file. Deliberate: the alternative is
  losing it silently.
- **`repos.json` is not hot-reloaded.** It is read once at startup and
  rewritten only by `PUT` / `DELETE` — never by a sync, a timer or a job. Hand-
  edit it while the app is closed. Entries the host cannot parse are skipped,
  reported in `registry_error`, and **written back verbatim** on every
  subsequent rewrite, so a typo never costs you the rest of your file. A file
  whose bytes it has but cannot use is renamed to
  `repos.json.corrupt-<epoch_ms>` and the host still starts, writable, with
  zero repos.

  A file it could not **read** — a permission it lacks, an I/O error, a
  directory sitting at that path — is the one case that stops writes. The host
  still starts, `GET /api/git` carries the `registry_error` saying so, and
  `PUT` / `DELETE` answer `403 registry_read_only` until the file is readable
  or moved aside. That is a real availability trade: a child that re-declares
  its repos with a boot-time `PUT` gets a hard 403 and runs with zero repos
  until a human intervenes, where before it "worked" by renaming a temp file
  over the definitions and credentials it could not read. On Windows, a backup
  agent or scanner briefly holding the file is enough to trip it, and since the
  registry is not hot-reloaded the lock-out lasts until the next restart.
  The `registry_error` is scrubbed on its way out, so what you read there is
  not byte-identical to what is in your file — a URL's `user:password@` is
  gone.
- **Everything is logged** to `<data-dir>/logs/git.log`, one line per outcome,
  rotating at 5 MB into `git.log.old`:

  ```
  <epoch_ms> <op> repo=<id> job=<job_id> ok  code=-      <message>
  <epoch_ms> <op> repo=<id> job=<job_id> err code=<code> <message>
  ```

  URLs are stripped of their userinfo and stored secrets are replaced with
  `***` before anything is written. That covers the `startup` lines too (`op`
  is `startup`, `job` is `-`), which are built from `repos.json` itself: a
  rejected entry quoting a remote with a password in it is reported with the
  password gone, and with the host still there so the line stays debuggable.
  One shape it cannot catch: a secret typed into a field that is not a URL —
  `"auto_sync_secs": "ghp_…"` — comes back inside serde's own "invalid type"
  complaint, because nothing at load time knows that string was meant to be a
  token. Worth caring about, because unlike `repos.json` the log files are
  created under the ordinary umask rather than forced to `0600`.
- **Windows.** `<data-dir>/repos/<id>/` plus a deep repository path can cross
  `MAX_PATH`, so keep ids short. A filename that is not valid UTF-8 produces an
  explicit `io_failed` rather than a silently mangled path. The §9.2 collision
  check is the filesystem's, not git's, so it is case-insensitive here and on
  macOS and case-sensitive on Linux: a local `Inbox.md` against a remote
  `inbox.md` refuses on two platforms out of three. A local-path `remote` must
  also carry a drive or UNC prefix here (`C:\repos\x.git`,
  `\\server\share\x.git`): a bare `/srv/repos/x.git` is drive-*relative* on
  Windows, so it is refused `invalid_request` rather than stored as something
  that would resolve against whatever the current drive happened to be.
- **Network integration tests.** Four `#[ignore]`d tests in `src/git/mod.rs`
  cover what an offline CI cannot. Run them by hand:

  ```bash
  # No credentials needed: the vendored-TLS canary and the credential retry cap.
  cargo test --bin hitch git::tests:: -- --ignored --nocapture

  # The credentialed pair, against a scratch repository you own.
  GIT_TEST_HTTPS_URL=https://github.com/you/scratch.git \
  GIT_TEST_TOKEN=github_pat_... \
  GIT_TEST_SSH_URL=git@github.com:you/scratch.git \
  GIT_TEST_SSH_KEY=$HOME/.ssh/id_ed25519 \
  cargo test --bin hitch git::tests:: -- --ignored --nocapture
  ```

  The scratch repository must be non-empty and have a `main` branch; the HTTPS
  test pushes a commit to it. `GIT_TEST_SSH_KEY` must be an unencrypted private
  key. CI runs `cargo test --locked` with no secrets and no network, so none of
  these ever runs there.
- **Don't need git at all?** See the removal recipe at the end of §7.
