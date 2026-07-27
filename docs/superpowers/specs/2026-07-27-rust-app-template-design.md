# Rust Chrome-Host Desktop App Template — Design

- **Date:** 2026-07-27
- **Status:** Approved in brainstorm; pending final user review of this document

## Purpose

A template for building cross-platform desktop apps (Windows, macOS, Linux) where a
Rust "host" binary displays a configured URL rendered by **Google Chrome**. An app
developer clones the template, edits one config file (`app.toml`), and builds a
distributable app. A fresh, unedited clone builds and runs immediately, showing a
built-in placeholder page that says the app is ready to be configured.

## Decisions made during brainstorm

| Question | Decision |
|---|---|
| Rendering engine | Google Chrome is a hard requirement (launched in `--app` mode). Not Tauri's native webview. |
| Chrome strictness | Google Chrome (stable) only. No Chromium/Edge/Brave fallback. |
| Platforms | Windows, macOS, Linux — all three. |
| Framework | **Pure Rust host, no app framework.** Tauri rejected: it renders exclusively through OS-native webviews (WebView2/WKWebView/webkit2gtk) and never uses Chrome, so its webview machinery would be dead weight, and on Linux it would force a `libwebkit2gtk-4.1` runtime dependency for an app that renders in Chrome. |
| Menu location | System tray icon (Windows tray / macOS menu-bar extras / Linux app indicator). Chrome `--app` windows have no controllable menu bar. |
| Config model | Single build-time `app.toml`, embedded into the binary via `include_str!`. |
| Server readiness | HTTP health-check poll against a configured URL, with timeout. |
| Settings UI | Schema-driven: fields declared in `app.toml`, form auto-generated and served by the host's internal server, opened in a second Chrome app window. |
| Settings consumption | Env vars (`APP_SETTING_<KEY>`) injected at server spawn + `settings.json` in the app-data dir. Changes apply via app restart. |
| Missing-Chrome UX | Native dialog → [Download Chrome] opens https://www.google.com/chrome in the default browser → app exits. User relaunches after installing. |
| Distribution | Single binary for dev; GitHub Actions release workflow builds installers per platform via `cargo-packager`. |

## Architecture

One Rust binary with four components, each its own module:

1. **Internal server** (`axum`, bound to `127.0.0.1` on an ephemeral port) — always
   runs. Serves the loading page, the placeholder page, the settings UI, and a small
   status API. All HTML/CSS assets embedded in the binary.
2. **Chrome manager** — detects Google Chrome per platform and launches it in app
   mode with a dedicated profile directory. The dedicated `--user-data-dir` is
   load-bearing: it forces a separate Chrome process owned by the host, so window
   close is observable as child-process exit and cleanup is reliable. Without it,
   Chrome delegates to any running instance and the child exits immediately.
3. **App-server supervisor** — spawns the configured server command with settings
   env vars injected, captures stdout/stderr to a log file, polls the health-check
   URL until ready or timeout, and kills the process on shutdown.
4. **Tray menu** (`tray-icon` + `muda` on the main-thread `tao` event loop; `tokio`
   runs async work on background threads). Menu contents, top to bottom:
   "Open <App>" (only while in tray mode with the window closed), custom items
   from config (each `open_url` opens in the default browser), "Settings…" (when
   enabled), "Restart App", separator, "Quit".

### Lifecycle

```
launch → single-instance check (already running? dialog + exit)
       → detect Chrome ──missing──→ dialog [Download Chrome | Quit] → exit
       → start internal server
       → [server] configured?
            yes → spawn server cmd → open Chrome at internal /loading
                  loading page polls /api/status; host polls health-check URL
                  healthy → loading page redirects to configured URL
            no  → open Chrome directly at configured URL
                  (no URL either → internal /placeholder page)
       → tray active throughout
exit paths:
  - Chrome window closed (child exit) → on_close = "quit": kill server, exit
                                        on_close = "tray": keep server; tray gains "Open <App>"
  - tray Quit → kill Chrome + server children, exit
```

## Configuration (`app.toml`)

Embedded at compile time; parsed and validated at startup (fail fast with a clear
error dialog on invalid config).

```toml
[app]
name = "My App"                      # window title, tray tooltip
identifier = "com.example.myapp"     # reverse-domain; names the app-data dir
url = ""                             # target URL; empty → placeholder page

[server]                             # omit entire section if no local server
command = ["node", "server.js"]      # argv array (no shell parsing)
cwd = "server"                       # see path resolution note below
env = { PORT = "8080" }
health_check_url = "http://127.0.0.1:8080/"
startup_timeout_secs = 60

[window]
width = 1200
height = 800
on_close = "quit"                    # "quit" | "tray"

[menu]
settings = false                     # true → Settings tray item + settings window
items = [{ id = "docs", label = "Documentation", open_url = "https://example.com/docs" }]

[[settings.fields]]                  # only read when menu.settings = true
key = "theme"
label = "Theme"
type = "select"                      # "text" | "boolean" | "select"
default = "light"
options = ["light", "dark"]          # required for "select", forbidden otherwise
```

Validation rules:

- `identifier` must be non-empty reverse-domain form.
- If `[server]` is present, `command` (non-empty) and `health_check_url` are required.
- If `menu.settings = true`, at least one `[[settings.fields]]` entry is required.
- `settings.fields[].key` must be `[A-Za-z0-9_]+` and unique ignoring case
  (keys are uppercased into `APP_SETTING_<KEY>`).
- `on_close` must be `"quit"` or `"tray"`.

Path resolution for `server.cwd`: absolute paths used as-is; relative paths
resolve against the executable's directory in release builds, and against the
project root (`CARGO_MANIFEST_DIR`) in debug builds so `cargo run` works from a
fresh clone.

Template default: `url = ""` and no `[server]` section → placeholder mode.

Runtime data lives in the OS app-data directory under `identifier`
(e.g. `%APPDATA%`, `~/Library/Application Support`, `~/.local/share`):
`settings.json`, `chrome-profile/`, `logs/`.

## Chrome detection & launch

Detection order per platform (first hit wins):

- **Windows:** registry `App Paths\chrome.exe` (HKLM, then HKCU), then
  `%ProgramFiles%\Google\Chrome\Application\chrome.exe`,
  `%ProgramFiles(x86)%\...`, `%LocalAppData%\Google\Chrome\Application\chrome.exe`.
- **macOS:** `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome`, then
  `~/Applications/...`.
- **Linux:** `google-chrome`, `google-chrome-stable` on `PATH`, then
  `/opt/google/chrome/chrome`.

Launch flags:

```
--app=<url>
--user-data-dir=<app-data>/chrome-profile
--no-first-run
--no-default-browser-check
--window-size=<width>,<height>
```

Detection takes an injected existence-check closure so tests can supply fake
filesystem/registry lookups.

## Internal server routes

| Route | Purpose |
|---|---|
| `GET /loading` | Spinner page with app name; polls `/api/status`, redirects to the target URL when ready; flips to an error view on timeout/failure. |
| `GET /placeholder` | "This app is ready to be configured" page pointing at `app.toml` and the README. |
| `GET /settings` | Auto-generated settings form from the schema (only when enabled). |
| `POST /api/settings` | Validates against schema, writes `settings.json`, responds with "restart to apply" state. |
| `POST /api/restart` | Restarts children (Chrome + server) to apply settings. |
| `GET /api/status` | `{ state: "starting" \| "ready" \| "error", target_url, message?, log_path? }` |

The internal server binds `127.0.0.1` only. Mutating endpoints require a per-launch
random token (embedded in the pages the host serves) so other local processes
can't drive them.

## Settings

- Tray → "Settings…" opens a second Chrome app window at internal `/settings`
  (same profile dir — becomes a window of the Chrome process the host owns).
- Form fields render from `[[settings.fields]]`; current values from
  `settings.json`, falling back to defaults.
- Save → POST → validate → write `settings.json` → page shows "Saved — restart to
  apply" with a Restart App button (restart also available in the tray).
- Restart = kill Chrome + server children, respawn both; server gets fresh env.
- Env contract at server spawn: `APP_SETTING_<KEY>` (key uppercased) per field,
  plus `APP_SETTINGS_FILE=<path to settings.json>`.

## Error handling

| Failure | Behavior |
|---|---|
| Chrome not installed | Dialog → [Download Chrome] opens download page in default browser → exit |
| Invalid embedded config | Dialog with validation error → exit |
| Server command fails to spawn | Dialog with error + log path → exit |
| Health check timeout | Loading page flips to error view with [Retry] + log path; the server child is left running until the user retries (which restarts the children) or quits from the tray |
| Server crashes mid-session | Dialog: "local server stopped unexpectedly" → [Restart] / [Quit] |
| Chrome window closed | Per `on_close`: quit (default) or keep running in tray with "Open <App>" |
| Second app launch | Dialog: "already running" → exit (lock file in app-data dir) |

Host log + captured server stdout/stderr → `<app-data>/logs/`, size-capped simple
rotation.

## Repo layout

```
rust-app-template/
├── app.toml                  # the one file an app developer edits
├── Cargo.toml
├── src/
│   ├── main.rs               # tao event loop, wiring, lifecycle state machine
│   ├── config.rs             # include_str!("../app.toml") + serde + validation
│   ├── chrome.rs             # detection, launch, process monitoring
│   ├── supervisor.rs         # server child: spawn, env injection, health poll, kill
│   ├── internal_server.rs    # axum routes
│   ├── settings.rs           # schema → form model, settings.json persistence
│   ├── tray.rs               # tray-icon + muda menu from config
│   └── assets/               # embedded HTML/CSS + tray icon
├── icons/                    # source icons for installers
├── .github/workflows/
│   ├── ci.yml                # fmt + clippy + test on 3-OS matrix
│   └── release.yml           # tag push → cargo-packager → NSIS, DMG, deb/AppImage
├── README.md                 # clone → edit app.toml → build walkthrough
└── LICENSE
```

Key crates: `tray-icon`, `muda`, `tao`, `rfd` (dialogs), `axum`, `tokio`,
`serde`/`toml`, `reqwest` (health poll), `dirs` (app-data paths),
`cargo-packager` (packaging tool, not a dependency).

## Packaging & CI

- `cargo run` on a fresh clone: placeholder page in a Chrome app window, working tray.
- `ci.yml`: rustfmt, clippy (deny warnings), tests on ubuntu/macos/windows runners.
- `release.yml`: on tag push, build with `cargo-packager` → NSIS (Windows),
  DMG/.app (macOS), deb/AppImage (Linux); upload as GitHub release artifacts.
- Code signing & notarization: out of scope; documented as per-app TODOs in README.
- Linux runtime dependency: `libayatana-appindicator` for the tray (documented).

## Testing

- Config parsing/validation: good + every invalid case above.
- Chrome detection: injectable lookup trait with fake paths/registry.
- Health-check polling: against a throwaway local HTTP server (success, slow-start,
  timeout).
- Settings round-trip: schema → rendered form model → POST → `settings.json` →
  env-var map.
- Chrome-launch integration test exists but is `#[ignore]`d (CI runners don't
  guarantee Chrome).

## Out of scope (explicitly)

- Auto-update.
- Code signing / notarization (documented, not implemented).
- Chromium/Edge/Brave fallback rendering.
- In-page menu UI; native in-window menu bars.
- Multiple app windows beyond main + settings.
- Mobile targets.
