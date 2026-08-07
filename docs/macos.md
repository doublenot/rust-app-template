# Hitch on macOS

Two things live here: a checklist for **testing a published release**, and notes
on **building and running from source**. Both matter more on macOS than on the
other platforms, because macOS is the only one where the shipped binary is
assembled from two builds and then signed.

Nothing in this document requires an Apple developer account.

---

## 1. Why this needs testing at all

The macOS `.dmg` is the only artifact this project cannot fully verify in CI.

The build is a **universal binary**: the release workflow builds
`aarch64-apple-darwin` and `x86_64-apple-darwin` on the arm64 runner and combines
them with `lipo`, so one download serves both Apple Silicon and Intel. `lipo`
output carries only the minimal signature the Apple linker applies, and recent
macOS refuses to run linker-signed-only binaries on Apple Silicon — so the
workflow ad-hoc signs the result with `codesign --force --sign -`.

CI proves the *build* side of that: it runs `lipo -info` and `codesign --verify`
in the job and both pass. What CI cannot prove is that the app **launches**. That
claim is marked **unverified** in
`docs/superpowers/specs/2026-08-07-release-pipeline-design.md` §5.4, and §2 of
this document is what closes it.

---

## 2. Testing a published release

### 2.1 Download and verify

```bash
cd ~/Downloads
VER=0.1.2
curl -LO "https://github.com/doublenot/rust-app-template/releases/download/v$VER/Hitch_${VER}_universal.dmg"
curl -LO "https://github.com/doublenot/rust-app-template/releases/download/v$VER/SHA256SUMS"
shasum -a 256 -c SHA256SUMS --ignore-missing
```

Use `shasum -a 256`, not `sha256sum` — macOS ships the Perl implementation from
`Digest::SHA`, not GNU coreutils. `--ignore-missing` lets you check the one file
you downloaded instead of requiring all four.

Expected: `Hitch_0.1.2_universal.dmg: OK`.

If your `shasum` is old enough to reject `--ignore-missing`, compare by hand
instead — this works on every version:

```bash
shasum -a 256 "Hitch_${VER}_universal.dmg"
grep 'universal\.dmg$' SHA256SUMS
```

The two hashes must match.

### 2.2 Install

Open the `.dmg` and drag **Hitch** to Applications.

### 2.3 Inspect the build before launching

These three say whether the *artifact* is right, independently of whether it
runs:

```bash
APP="/Applications/Hitch.app"

lipo -info "$APP/Contents/MacOS/hitch"
# want: Architectures in the fat file: ... are: x86_64 arm64

codesign -dv "$APP" 2>&1 | grep -i '^Signature'
# want: Signature=adhoc

spctl -a -vv "$APP"
# want: rejected
```

**`spctl` rejecting is the correct result.** It reports that the app is not
notarized, which is exactly what this template ships, and it is why the first
launch needs §2.4 rather than a double-click. A *pass* here would mean something
unexpected had signed the app.

### 2.4 Launch

Right-click **Hitch** → **Open**, then **Open** in the dialog that appears. A
plain double-click will be refused the first time; after one successful
right-click launch, macOS remembers and double-click works.

Google Chrome must be installed — it is a runtime requirement on every platform,
though never a build-time one.

**What success looks like:** a Chrome window showing Hitch's built-in placeholder
page, saying no URL is configured, plus a tray icon in the menu bar. That is
correct, not a failure: `app.toml` ships with `url = ""`, so a fresh build has
nothing to point at yet.

### 2.5 What the outcomes mean

| what happens | meaning |
|---|---|
| Opens, placeholder page, tray icon | The shipped artifact works. §5.4's reasoning holds and can be promoted from *unverified*. |
| "can't be opened", or it dies instantly with no window | The ad-hoc signature did not survive packaging. Reopen §5.4. |
| "Hitch is damaged and should be moved to the Trash" | Worse than unnotarized — the bundle reads as unsigned. |
| Window opens but no tray icon | Unrelated to signing; a tray problem worth its own investigation. |

### 2.6 Optional: prove both architecture slices really run

`lipo -info` only proves both slices are *present*. This proves they execute:

```bash
arch -arm64  "$APP/Contents/MacOS/hitch"
arch -x86_64 "$APP/Contents/MacOS/hitch"   # needs Rosetta
```

Each should start the app rather than report a bad CPU type. On Apple Silicon
without Rosetta, install it first:

```bash
softwareupdate --install-rosetta
```

### 2.7 What to report back

Useful to capture:

- the exact output of the three commands in §2.3
- whether the app launched, and the **exact wording** of any dialog
- `sw_vers` (macOS version) and `uname -m` (`arm64` or `x86_64`)

If it fails to launch, this usually says why:

```bash
log show --last 5m --predicate 'process == "hitch"' --info 2>/dev/null | tail -40
```

A Gatekeeper or signature kill also shows up here:

```bash
log show --last 5m --predicate 'subsystem == "com.apple.syspolicy"' --info | tail -20
```

---

## 3. Building and running from source

### 3.1 Prerequisites

- **Rust** stable, `rust-version` 1.85 or newer.
- **Xcode Command Line Tools** — `xcode-select --install`. This supplies `lipo`,
  `codesign`, and the `perl` and `make` that the vendored OpenSSL needs.
- **Google Chrome**, at runtime.

Unlike Linux, macOS needs no extra system packages: no GTK, no appindicator, and
the tray comes from the OS.

### 3.2 Run it

```bash
cargo run
```

This builds the host, validates the embedded `app.toml`, starts the internal
loopback server, optionally launches and health-checks `[server]`, and opens
Chrome in app mode.

### 3.3 Build a universal `.dmg` locally

The same sequence the release workflow runs, if you want to reproduce an
installer without cutting a tag:

```bash
cargo install cargo-packager --version 0.11.8 --locked
rustup target add x86_64-apple-darwin

bin=$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[0].default_run')
cargo build --release --locked --target aarch64-apple-darwin
cargo build --release --locked --target x86_64-apple-darwin

mkdir -p target/universal-apple-darwin/release
lipo -create -output "target/universal-apple-darwin/release/$bin" \
  "target/aarch64-apple-darwin/release/$bin" \
  "target/x86_64-apple-darwin/release/$bin"

# Required, not optional -- see §1.
codesign --force --sign - "target/universal-apple-darwin/release/$bin"

cargo packager --release --target universal-apple-darwin --formats dmg --verbose
```

The result lands in `target/universal-apple-darwin/release/`, **not**
`target/release/` — supplying a target triple moves cargo-packager's output
directory. A single-architecture `cargo packager --release --formats dmg` is
faster for a quick check, but produces an Apple-Silicon-only `.dmg`.

You can also exercise all three platforms' builds without cutting a tag by
running the Release workflow from the Actions tab against a branch: it builds
every leg and uploads the installers as workflow artifacts without touching a
release. See README §8.

---

## 4. Signing, properly

Ad-hoc signing makes the app **run**; it does not make it **trusted**. Users will
keep seeing the unidentified-developer warning until the app is signed with a
Developer ID certificate and notarized by Apple, which needs a paid Apple
Developer account.

That is deliberately left as a per-app step. When you do it, `cargo-packager`
reads a signing identity from `[package.metadata.packager.macos]` and will sign
and notarize the bundle for you — at which point `spctl -a -vv` in §2.3 should
start *accepting* the app, and the right-click dance in §2.4 goes away.

- <https://developer.apple.com/documentation/security/notarizing_macos_software_before_distribution>
