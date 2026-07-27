# Chrome Host App

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
  sudo apt-get install libgtk-3-dev libayatana-appindicator3-dev libxdo-dev
  ```

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
   when no settings schema is configured).
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
| `logs/` | `server.log` (+ `.old`) for the supervised `[server]` child, `host.log` (+ `.old`) for the host itself, `chrome.log` (+ `.old`) for Chrome's own stdout/stderr (GPU probes, service chatter — kept out of the host's terminal). All three rotate at 5 MB. |
| `settings.json` | Persisted settings values, keyed by settings field `key`. |
| `app.lock` | Single-instance lock file; prevents two copies of the app running against the same identifier at once. |

## 7. Building and packaging

### Building on each OS

The same commands build everywhere once the per-OS prerequisites are in
place:

```bash
cargo build --release   # optimized binary in target/release/
cargo run               # debug build + launch (development)
```

The release binary lands at `target/release/chrome-host-app`
(`chrome-host-app.exe` on Windows) and embeds `app.toml`, the internal
pages, and the icon — it is self-contained apart from Google Chrome itself
and, on Linux, the tray runtime library.

| OS | toolchain | extra build dependencies |
|---|---|---|
| Linux (Debian/Ubuntu) | Rust 1.85+ via [rustup](https://rustup.rs) | `sudo apt-get install libgtk-3-dev libayatana-appindicator3-dev libxdo-dev` (same list as §2) |
| macOS | Rust 1.85+ via rustup, plus Xcode Command Line Tools (`xcode-select --install`) | none |
| Windows | Rust 1.85+ via rustup with the default MSVC toolchain (requires [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) with "Desktop development with C++") | none |

Google Chrome is a **runtime** requirement on every OS, never a build-time
one. Cross-compiling between OSes is not supported — build on each target
OS, or let CI do it: `.github/workflows/ci.yml` builds and tests all three
platforms on every push, and the release workflow packages installers for
all three on every `v*` tag (see §8).

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

## 8. Releasing

Pushing a tag matching `v*` (e.g. `v1.0.0`) triggers the GitHub Actions
release workflow (`.github/workflows/release.yml`), which runs
`cargo build --release --locked` followed by `cargo packager --release` on
each supported OS runner and attaches the resulting installers to the
GitHub release for that tag.
