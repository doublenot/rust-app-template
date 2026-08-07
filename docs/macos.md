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
in the job and both pass. What CI cannot prove is that the app **launches**.

That gap was not theoretical. **v0.1.0 through v0.1.2 shipped a broken bundle:**
the workflow signed the binary but nothing signed `Hitch.app` itself, and since
Gatekeeper validates the bundle rather than the executable inside it, a
downloaded copy reported *"Hitch is damaged and should be moved to the Trash."*
CI could not have caught it — `codesign --verify` ran against the binary, which
was never the problem. It took someone installing the app.

Fixed in **v0.1.3** by having cargo-packager sign the bundle
(`signing-identity = "-"`), and confirmed on the same Mac that found it: the app
is now blocked as an unidentified developer and opens once allowed, instead of
being declared damaged. The post-mortem is §5.8 of
`docs/superpowers/specs/2026-08-07-release-pipeline-design.md`.

**So: use v0.1.3 or later.** Everything below applies to any version, but the
signature checks in §2.3 will fail on v0.1.2 and earlier by design.

---

## 2. Testing a published release

### 2.1 Download

**How you download decides what you are able to test.** macOS attaches the
`com.apple.quarantine` extended attribute to files fetched by browsers and other
quarantine-aware apps. Gatekeeper only engages on quarantined files — so a
`.dmg` fetched with `curl`, `wget` or `gh` opens with **no prompt at all**,
regardless of whether it is signed.

| method | quarantined? | good for |
|---|---|---|
| Browser (Safari, Chrome, Firefox) | yes | testing Gatekeeper — this is what a user actually does |
| `curl` / `wget` / `gh release download` | **no** | checksums, inspecting the build, scripting |

For a Gatekeeper test, download from the
[releases page](https://github.com/doublenot/rust-app-template/releases) in a
browser, then confirm the flag is really there before you install:

```bash
xattr -p com.apple.quarantine ~/Downloads/Hitch_*_universal.dmg
```

A value printed means the test is real. `No such xattr` means the download
bypassed Gatekeeper and any result you get is meaningless.

#### Terminal download, for checksums and inspection

```bash
cd ~/Downloads
VER=0.1.3
BASE=https://github.com/doublenot/rust-app-template/releases/download/v$VER
curl -LO "$BASE/Hitch_${VER}_universal.dmg"
curl -LO "$BASE/SHA256SUMS"
shasum -a 256 -c SHA256SUMS --ignore-missing
```

Use `shasum -a 256`, not `sha256sum` — macOS ships the Perl implementation from
`Digest::SHA`, not GNU coreutils. `--ignore-missing` lets you check the one file
you downloaded instead of requiring all four.

Expected: `Hitch_0.1.3_universal.dmg: OK`.

If your `shasum` is old enough to reject `--ignore-missing`, compare by hand
instead — this works on every version:

```bash
shasum -a 256 "Hitch_${VER}_universal.dmg"
grep 'universal\.dmg$' SHA256SUMS
```

The two hashes must match.

#### Making a terminal download testable

If you would rather stay in the terminal, apply the quarantine flag yourself.
The attribute is `flags;date;agent;UUID`, and `0081` is the code for "has not
been executed yet":

```bash
xattr -w com.apple.quarantine \
  "0081;$(printf %x $(date +%s));Safari;$(uuidgen)" \
  "Hitch_${VER}_universal.dmg"

xattr -p com.apple.quarantine "Hitch_${VER}_universal.dmg"   # confirm it took
```

The flag propagates to the `.app` when you copy it out of the mounted image, so
the rest of §2 then behaves exactly as it would for a browser download. This is
a testing convenience, not a security measure — you are re-creating a check you
could equally have skipped.

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
# On v0.1.2 and earlier this instead reported that the object is not signed at
# all -- that was the "damaged" bug (§1).

codesign --verify --deep --strict -v "$APP"
# want: valid on disk / satisfies its Designated Requirement
# This checks the BUNDLE. It is the check that was missing.

spctl -a -vv "$APP"
# want: rejected
```

**`spctl` rejecting is the correct result.** It reports that the app is not
notarized, which is exactly what this template ships, and it is why the first
launch needs §2.4 rather than a double-click. A *pass* here would mean something
unexpected had signed the app.

### 2.4 Launch

This app is **not notarized**, so macOS will refuse the first launch. How you
allow it depends on the OS version, and the older advice no longer works:

**macOS 15 (Sequoia) and later.** Apple removed the Control-click → Open
bypass. Double-click Hitch, let it be blocked, then go to **System Settings →
Privacy & Security**, scroll to the message naming Hitch, click **Open Anyway**,
and confirm with your password. Only the first launch needs this.

**macOS 14 (Sonoma) and earlier.** Right-click **Hitch** → **Open**, then
**Open** in the dialog.

**Either version, from the terminal** — removes the quarantine flag macOS
attaches to downloads, which is what triggers the check at all:

```bash
xattr -dr com.apple.quarantine "/Applications/Hitch.app"
```

Do that only for software you have reason to trust; it is the same override the
GUI performs, without the dialog.

Google Chrome must be installed — it is a runtime requirement on every platform,
though never a build-time one.

**What success looks like:** a Chrome window showing Hitch's built-in placeholder
page, saying no URL is configured, plus a tray icon in the menu bar. That is
correct, not a failure: `app.toml` ships with `url = ""`, so a fresh build has
nothing to point at yet.

### 2.5 What the outcomes mean

| what happens | meaning |
|---|---|
| Blocked as an unidentified developer, then opens via §2.4 | Correct and expected. The app is signed but not notarized, which is what this template ships. |
| Opens, placeholder page, tray icon | Working. |
| **"Hitch is damaged and should be moved to the Trash"** | The bundle is unsigned. This is the v0.1.0–v0.1.2 bug (§1); on a later build it means the fix regressed. Check `codesign -dv` in §2.3 — it will say the object is not signed at all. |
| "can't be opened", or it dies instantly with no window | The signature did not survive packaging. Spec §5.4 / §5.8. |
| Window opens but no tray icon | Unrelated to signing; a tray problem worth its own investigation. |

A block is not a failure. An unnotarized app is *supposed* to be refused on first
launch — §2.4 is how you allow it. The failure mode to care about is "damaged",
which means macOS could not validate the code at all.

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
