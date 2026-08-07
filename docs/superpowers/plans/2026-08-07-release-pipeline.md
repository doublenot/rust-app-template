# Release Pipeline Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `.github/workflows/release.yml` — which exists, is documented,
and has never run — with a tag-driven pipeline that gates on the crate version,
builds installers on three runners, and creates exactly one draft GitHub release
carrying all four installers plus `SHA256SUMS`.

**Architecture:** Three jobs. `verify` compares the tag to `Cargo.toml` and fails
in about ten seconds. `package` is a 3-OS matrix that builds, packages, stages
artifacts into a flat `dist/`, and uploads them as workflow artifacts. `publish`
runs only for a tag ref, downloads every leg's artifacts, computes one
`SHA256SUMS`, and creates a single draft release. A `workflow_dispatch` trigger
runs the first two jobs without touching a release. Supporting work:
`scripts/release.sh` makes bump + lockfile + commit + tag one command, and its
guards get a real test.

**Tech Stack:** GitHub Actions, `cargo-packager` 0.11.8, `lipo`/`codesign`
(macOS), bash, python3 (stdlib only), `jq`.

**Spec:** `docs/superpowers/specs/2026-08-07-release-pipeline-design.md`. Section
references below (§2, §5.4, …) point into it.

## Global Constraints

- **`cargo-packager` is pinned to `0.11.8`**, via `PACKAGER_VERSION` at workflow
  level. The output filenames are the contract between the `package` and
  `publish` jobs, and they come from format strings inside the tool.
- **No new Rust dependencies.** Nothing in this plan touches `[dependencies]`.
  `rust-version = "1.85"` and `edition = "2021"` are unchanged.
- **The binary name is never hardcoded.** This repository is a template. Derive
  it from `cargo metadata --no-deps … .packages[0].default_run`, which returns
  `chrome-host-app` and is the same key `cargo build` uses to disambiguate the
  two bin targets (`gen_icons` is the other).
- **The four artifact names** (§2), which every glob and count below depends on:
  | format | filename | directory |
  |---|---|---|
  | deb | `chrome-host-app_0.1.0_amd64.deb` | `target/release/` |
  | AppImage | `chrome-host-app_0.1.0_x86_64.AppImage` | `target/release/` |
  | NSIS | `chrome-host-app_0.1.0_x64-setup.exe` | `target/release/` |
  | dmg | `Chrome Host App_0.1.0_universal.dmg` | `target/universal-apple-darwin/release/` |

  The `.dmg` is the only one named from `product-name`, so the only one with
  **spaces**, and under a universal build the only one outside `target/release/`.
  Quote every expansion that touches it.
- **`scripts/*.sh` must be committed executable** (`git update-index --chmod=+x`
  if the working filesystem loses the bit).
- **Comments explain why, not what**, matching the density of `src/supervisor.rs`
  and `.github/workflows/ci.yml`. Every non-obvious line below carries the reason
  it cannot be simplified away.

---

## File Structure

| file | status | responsibility |
|---|---|---|
| `scripts/release.sh` | create | Bump `[package].version`, sync `Cargo.lock`, commit, tag. Never pushes. |
| `scripts/test-release.sh` | create | Asserts every guard in `release.sh`, against throwaway git repos. |
| `.github/workflows/ci.yml` | modify | One step so `test-release.sh` runs on the Linux leg and cannot rot. |
| `.github/workflows/release.yml` | replace | The three-job pipeline. |
| `README.md` | modify | §7 (pinned packager version), §8 (rewritten releasing section). |

Task 1 is self-contained and testable on this machine. Task 2 cannot be tested
here at all — its test is Task 4, which runs it on real runners. Task 3 depends
on both being final so the documentation describes what shipped.

---

## Task 1: `scripts/release.sh` and its test

**Files:**
- Create: `scripts/release.sh`
- Create: `scripts/test-release.sh`
- Modify: `.github/workflows/ci.yml` (after the `cargo test --locked` step)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `scripts/release.sh <version>` — creates commit `chore: release v<version>`
  and annotated tag `v<version>` in the current repository. Exit 0 on success,
  exit 1 with a `release: <reason>` message on stderr for every refusal. Task 3
  documents this command; Task 4 does not use it.

- [ ] **Step 1: Write the failing test**

Create `scripts/test-release.sh`:

```bash
#!/usr/bin/env bash
# Exercises scripts/release.sh against throwaway git repositories.
#
# Every refusal below is a guard whose absence produces a bad release: a tag that
# captures unrelated work, a lockfile that disagrees with the manifest, a version
# rewritten in the wrong table. Asserting them is cheaper than discovering one
# during a release.
#
# Deliberately not `set -e`: a failing assertion should be counted and reported,
# not abort the run.
set -uo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
release_sh="$here/release.sh"
pass=0
fail=0

ok()     { pass=$((pass + 1)); printf '  ok      %s\n' "$1"; }
not_ok() { fail=$((fail + 1)); printf '  NOT OK  %s\n' "$1"; }
check()  { # check <description> <wanted-exit> <actual-exit>
  if [ "$2" -eq "$3" ]; then ok "$1"; else not_ok "$1 (wanted exit $2, got $3)"; fi
}
assert() { # assert <description> <command...>
  if "${@:2}" >/dev/null 2>&1; then ok "$1"; else not_ok "$1"; fi
}

# A dependency-free crate, so cargo needs no network and finishes instantly.
# Two fixtures earn their place: `keywords` is an array value inside [package]
# (a table-scoping regex that stops at the first '[' character truncates there),
# and [package.metadata.packager] carries a SECOND `version` key holding the same
# string as the real one (a naive substitution rewrites it).
fixture() {
  local d
  d=$(mktemp -d)
  mkdir -p "$d/src"
  cat > "$d/Cargo.toml" <<'EOF'
[package]
name = "fixture"
version = "0.1.0"
edition = "2021"
keywords = ["release", "test"]

[package.metadata.packager]
product-name = "Fixture"
version = "0.1.0"

[dependencies]
EOF
  echo 'fn main() {}' > "$d/src/main.rs"
  ( cd "$d" && cargo generate-lockfile --quiet ) >/dev/null 2>&1
  git -C "$d" init -q -b main
  git -C "$d" config user.email release-test@example.com
  git -C "$d" config user.name "Release Test"
  git -C "$d" add -A
  git -C "$d" commit -qm init
  echo "$d"
}

echo "release.sh"

d=$(fixture); ( cd "$d" && "$release_sh" >/dev/null 2>&1 )
check "refuses no argument" 1 $?; rm -rf "$d"

d=$(fixture); ( cd "$d" && "$release_sh" 1.2 >/dev/null 2>&1 )
check "refuses a non-semver version" 1 $?; rm -rf "$d"

d=$(fixture); ( cd "$d" && "$release_sh" 0.1.0 >/dev/null 2>&1 )
check "refuses the version it is already at" 1 $?; rm -rf "$d"

d=$(fixture); echo dirt > "$d/dirt.txt"; git -C "$d" add dirt.txt
( cd "$d" && "$release_sh" 1.2.0 >/dev/null 2>&1 )
check "refuses a dirty working tree" 1 $?; rm -rf "$d"

d=$(fixture); git -C "$d" tag -a v1.2.0 -m v1.2.0
( cd "$d" && "$release_sh" 1.2.0 >/dev/null 2>&1 )
check "refuses a tag that already exists" 1 $?; rm -rf "$d"

d=$(fixture); ( cd "$d" && "$release_sh" 1.2.0 >/dev/null 2>&1 )
check "bumps, commits, and tags" 0 $?

assert "[package].version is bumped"            grep -q '^version = "1.2.0"' "$d/Cargo.toml"
assert "an array value in [package] survives"   grep -q '^keywords = \["release", "test"\]' "$d/Cargo.toml"
assert "Cargo.lock is synced"                   grep -q '^version = "1.2.0"' "$d/Cargo.lock"
assert "the tag exists"                         git -C "$d" rev-parse -q --verify refs/tags/v1.2.0

# `git show --stat HEAD -- Cargo.lock` exits 0 whether or not the file is in the
# commit, so name the changed paths and match one exactly.
if git -C "$d" diff-tree --no-commit-id --name-only -r HEAD | grep -qx Cargo.lock; then
  ok "the commit carries Cargo.lock"
else
  not_ok "the commit carries Cargo.lock"
fi

# Exactly one `version = "0.1.0"` must remain: the one under
# [package.metadata.packager]. Two means nothing was bumped; zero means the edit
# escaped its table and rewrote the metadata too.
if [ "$(grep -c '^version = "0.1.0"' "$d/Cargo.toml")" -eq 1 ]; then
  ok "the [package.metadata] version is untouched"
else
  not_ok "the [package.metadata] version is untouched"
fi

if [ -z "$(git -C "$d" status --porcelain)" ]; then
  ok "the working tree is clean afterwards"
else
  not_ok "the working tree is clean afterwards"
fi
rm -rf "$d"

echo
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
```

- [ ] **Step 2: Run it to make sure it fails**

```bash
chmod +x scripts/test-release.sh
scripts/test-release.sh
```

Expected: fails immediately — `scripts/release.sh` does not exist, so every
`check` reports the wrong exit code (127, not 1 or 0) and the script exits
non-zero.

- [ ] **Step 3: Write `scripts/release.sh`**

```bash
#!/usr/bin/env bash
# Bump the crate version, sync Cargo.lock, commit, and tag.
#
# Deliberately does NOT push. Pushing the tag is what starts a release
# (.github/workflows/release.yml), so it stays a separate, deliberate act.
set -euo pipefail

die() { echo "release: $*" >&2; exit 1; }

[ $# -eq 1 ] || die "usage: $(basename "$0") <version>   e.g. $(basename "$0") 1.2.0"
new=$1

# The tag is v$new and the workflow's gate compares it to this string exactly,
# so a version cargo would accept but the gate would not must be refused here.
[[ $new =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]] || die "not a version: $new"

cd "$(git rev-parse --show-toplevel)"

# A dirty tree would put unrelated work inside the tagged commit.
[ -z "$(git status --porcelain)" ] || die "working tree is dirty"

cur=$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[0].version')
[ "$cur" != "$new" ] || die "already at $new"

! git rev-parse -q --verify "refs/tags/v$new" >/dev/null || die "tag v$new already exists"

# Scoped to the [package] table. `version = "0.1.0"` also appears under
# [package.metadata.packager], and a dependency could be pinned to the same
# string -- both are indistinguishable from the real one to sed. The lookahead is
# line-anchored (^\[) rather than a plain [^\[]* so an array value inside
# [package], such as `keywords = [...]`, does not truncate the table.
python3 - "$new" <<'PY'
import pathlib
import re
import sys

new = sys.argv[1]
path = pathlib.Path("Cargo.toml")
text = path.read_text()

table = re.search(r"(?ms)^\[package\]\s*\n.*?(?=^\[|\Z)", text)
if table is None:
    sys.exit("release: no [package] table in Cargo.toml")

patched, count = re.subn(
    r'(?m)^(version\s*=\s*)"[^"]*"',
    lambda m: m.group(1) + f'"{new}"',
    table.group(0),
    count=1,
)
if count != 1:
    sys.exit("release: no version key in the [package] table")

path.write_text(text[: table.start()] + patched + text[table.end() :])
PY

# Updates only this workspace member's entry -- a one-line diff, no compilation,
# and no drive-by bump of every dependency inside a release commit. The manifest
# and the lockfile must land together: `--locked` would fail on all three
# runners if they disagreed.
cargo update --workspace --quiet

git add Cargo.toml Cargo.lock
git commit -q -m "chore: release v$new"
git tag -a "v$new" -m "v$new"

echo "committed and tagged v$new"
echo "push when ready:  git push origin main && git push origin v$new"
```

- [ ] **Step 4: Run the test to verify it passes**

```bash
chmod +x scripts/release.sh
scripts/test-release.sh
```

Expected: `13 passed, 0 failed`, exit 0.

- [ ] **Step 5: Mutation-check the table scoping**

The `[package.metadata]` assertion is the one that justifies the python edit over
a one-line `sed`. Prove it can fail. Temporarily replace the python block with:

```bash
sed -i.bak "s/^version = \".*\"/version = \"$new\"/" Cargo.toml && rm -f Cargo.toml.bak
```

Run `scripts/test-release.sh`. Expected: `NOT OK  the [package.metadata] version is untouched`.
Then restore the python block and re-run to confirm it goes green again.

- [ ] **Step 6: Wire it into CI so it cannot rot**

In `.github/workflows/ci.yml`, immediately after the `- run: cargo test --locked`
step, add:

```yaml
      # Guards that only matter at release time rot silently otherwise; the
      # release helper is never exercised by `cargo test`.
      - name: Test the release helper
        if: runner.os == 'Linux'
        run: scripts/test-release.sh
```

- [ ] **Step 7: Verify the repo's own gate still passes**

```bash
cargo fmt --check && cargo clippy --all-targets --locked -- -D warnings && cargo test --locked
```

Expected: all three pass. (No Rust source changed; this confirms nothing was
disturbed.)

- [ ] **Step 8: Commit**

```bash
chmod +x scripts/release.sh scripts/test-release.sh
git add scripts/release.sh scripts/test-release.sh .github/workflows/ci.yml
git commit -m "feat: one command for the bump, and a test for every way it can go wrong

Bumping a version by hand means editing Cargo.toml, remembering that Cargo.lock
records the same number, committing both together, and tagging the result --
where a lockfile left behind fails --locked on all three runners, and a stray
tag is the thing the release workflow keys on.

The edit is scoped to the [package] table rather than sed'd across the file.
version = \"0.1.0\" appears twice in this manifest, and the second one is under
[package.metadata.packager]; a dependency pinned to the same string would be
just as indistinguishable. The test plants both that duplicate and an array
value inside [package], because a table-scoping regex that stops at the first
'[' character truncates on the latter."
```

---

## Task 2: The release workflow

**Files:**
- Replace: `.github/workflows/release.yml`

**Interfaces:**
- Consumes: nothing from Task 1 at runtime. (`release.sh` produces the tag a
  human pushes; the workflow only reads `Cargo.toml`.)
- Produces: a draft GitHub release for tag `v<version>` carrying four installers
  and `SHA256SUMS`. Task 3 documents it; Task 4 verifies it.

- [ ] **Step 1: Replace the workflow file**

Write `.github/workflows/release.yml`:

```yaml
name: Release
on:
  push:
    tags: ["v*"]
  # Dry run: builds every leg and uploads the installers as workflow artifacts
  # without touching a release. Only visible in the Actions tab once this file is
  # on the default branch.
  workflow_dispatch:

# Read by default; only `publish` is granted write. The build legs have no
# business holding a token that can rewrite the repository.
permissions:
  contents: read

env:
  PACKAGER_VERSION: "0.11.8"

jobs:
  verify:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      # Ten seconds here against ~30 runner-minutes wasted on a mismatch: without
      # it a v1.0.0 tag on an 0.1.0 manifest builds and ships artifacts named
      # 0.1.0, and nothing notices.
      - name: Tag must match the crate version
        run: |
          if [ "$GITHUB_REF_TYPE" != tag ]; then
            echo "dry run on $GITHUB_REF_NAME — skipping the version gate"; exit 0
          fi
          crate=$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[0].version')
          [ "v$crate" = "$GITHUB_REF_NAME" ] || {
            echo "::error::tag $GITHUB_REF_NAME != crate version $crate"; exit 1; }
          echo "tag $GITHUB_REF_NAME matches crate version $crate"

  package:
    needs: verify
    strategy:
      fail-fast: false
      matrix:
        include:
          - { os: ubuntu-latest,  formats: deb,appimage }
          - { os: macos-latest,   formats: dmg }
          - { os: windows-latest, formats: nsis }
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4

      # perl and pkg-config are for the vendored OpenSSL that libssh2-sys pulls
      # in on every unix target (git2 = vendored-libgit2 + ssh). macos-latest and
      # windows-latest need nothing extra: macOS gets perl and make from the
      # Xcode CLT, and Windows MSVC builds no OpenSSL at all.
      - name: Install Linux system deps
        if: runner.os == 'Linux'
        run: |
          sudo apt-get update
          sudo apt-get install -y libgtk-3-dev libayatana-appindicator3-dev libxdo-dev \
                                  perl pkg-config

      - uses: dtolnay/rust-toolchain@stable
      - name: Add the Intel target for the universal build
        if: runner.os == 'macOS'
        run: rustup target add x86_64-apple-darwin
      - uses: Swatinem/rust-cache@v2

      # cargo-packager publishes no prebuilt binaries anywhere -- its GitHub
      # releases carry zero assets -- so `cargo install` compiles it from source on
      # every leg of every run. Cache the built binary instead. The pin is not only
      # for the cache key: the artifact filenames staged below come from format
      # strings inside this tool, and an unpinned upgrade could rename them.
      - name: Cache cargo-packager
        id: packager-cache
        uses: actions/cache@v4
        with:
          path: ~/.cargo/bin/cargo-packager*
          key: cargo-packager-${{ env.PACKAGER_VERSION }}-${{ runner.os }}-${{ runner.arch }}
      - name: Install cargo-packager
        if: steps.packager-cache.outputs.cache-hit != 'true'
        run: cargo install cargo-packager --version ${{ env.PACKAGER_VERSION }} --locked

      - name: Build and package
        if: runner.os != 'macOS'
        shell: bash
        run: |
          cargo build --release --locked
          cargo packager --release --formats ${{ matrix.formats }} --verbose

      # macos-latest is arm64, so a plain build ships an Apple-Silicon-only .dmg and
      # Intel Macs get nothing. Build both targets here and lipo them into one.
      - name: Build, lipo, sign, and package (macOS universal)
        if: runner.os == 'macOS'
        run: |
          bin=$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[0].default_run')
          cargo build --release --locked --target aarch64-apple-darwin
          cargo build --release --locked --target x86_64-apple-darwin
          mkdir -p target/universal-apple-darwin/release
          lipo -create -output "target/universal-apple-darwin/release/$bin" \
            "target/aarch64-apple-darwin/release/$bin" \
            "target/x86_64-apple-darwin/release/$bin"
          # Mandatory, not cosmetic. lipo output carries only the minimal signature
          # the Apple linker applies, and recent macOS SIGKILLs linker-signed-only
          # binaries on Apple Silicon -- unsigned, this .dmg installs an app that
          # dies on launch rather than one Gatekeeper merely warns about.
          # cargo-packager never ad-hoc signs (it skips signing entirely without a
          # configured identity) and `--formats dmg` builds the .app in the same
          # pass, so there is no seam to sign the bundle in. It goes on the binary,
          # before packaging copies it into Contents/MacOS/.
          codesign --force --sign - "target/universal-apple-darwin/release/$bin"
          # In the log, not in a comment: this leg cannot be reproduced off a runner.
          lipo -info "target/universal-apple-darwin/release/$bin"
          codesign --verify --verbose "target/universal-apple-darwin/release/$bin"
          cargo packager --release --target universal-apple-darwin --formats dmg --verbose

      # Staged flat rather than uploaded by glob, for two independent reasons.
      # upload-artifact roots an artifact at the least common ancestor of its search
      # paths, so listing target/release/*.deb beside
      # target/universal-apple-darwin/release/*.dmg would root at target/ and keep
      # the nesting -- publish's dist/ would then hold directories, not files.
      # And an exact count catches a leg that emits one file where two were
      # expected, which no if-no-files-found setting can.
      #
      # nullglob is required, not decorative: without it an unmatched glob expands
      # to its own literal text, so `found` is non-empty and the macOS/Windows
      # count of 1 passes while cp fails on a path that does not exist.
      - name: Stage artifacts
        shell: bash
        run: |
          shopt -s nullglob
          case "${{ runner.os }}" in
            Linux)   want=2; found=(target/release/*.deb target/release/*.AppImage) ;;
            macOS)   want=1; found=(target/universal-apple-darwin/release/*.dmg) ;;
            Windows) want=1; found=(target/release/*-setup.exe) ;;
          esac
          printf 'found: %s\n' "${found[@]}"
          [ "${#found[@]}" -eq "$want" ] || {
            echo "::error::expected $want artifact(s) on ${{ runner.os }}, got ${#found[@]}"
            exit 1; }
          mkdir -p dist
          cp -- "${found[@]}" dist/

      - uses: actions/upload-artifact@v4
        with:
          name: package-${{ matrix.os }}
          if-no-files-found: error
          path: dist/

  publish:
    needs: package
    # Gated on ref_type rather than on the event. Dispatching against a *tag*
    # publishes -- the supported way to re-cut a release whose upload failed --
    # while the ordinary dispatch-against-a-branch dry run can never touch one.
    if: github.ref_type == 'tag'
    runs-on: ubuntu-latest
    permissions:
      contents: write
    steps:
      # `needs: package` is what makes a partial release impossible: fail-fast is
      # off so every leg's log survives, but one failure means this job never runs
      # and no half-populated draft exists to mistake for a finished one.
      - uses: actions/download-artifact@v4
        with:
          path: dist
          merge-multiple: true

      - name: Checksums
        run: |
          # Computed in a subshell so the redirect lands outside dist/. Do not
          # "simplify" this to find/xargs and do not assume dist/ is clean -- both
          # make SHA256SUMS list itself. The plain glob form is safe only because
          # pathname expansion completes before the redirect is performed, which
          # stops being true the moment a SHA256SUMS is already there.
          ( cd dist && sha256sum -- * ) > SHA256SUMS
          mv SHA256SUMS dist/
          cat dist/SHA256SUMS

      - uses: softprops/action-gh-release@v2
        with:
          draft: true
          files: dist/*
          fail_on_unmatched_files: true
          generate_release_notes: true
```

- [ ] **Step 2: Verify the YAML parses and the structure is what was intended**

```bash
python3 - <<'PY'
import yaml
d = yaml.safe_load(open(".github/workflows/release.yml"))
jobs = d["jobs"]
# `on:` parses as the boolean True in YAML 1.1 -- that is expected, not a bug.
trig = d[True]
assert set(trig) == {"push", "workflow_dispatch"}, trig
assert trig["push"]["tags"] == ["v*"]
assert d["permissions"] == {"contents": "read"}
assert d["env"]["PACKAGER_VERSION"] == "0.11.8"
assert list(jobs) == ["verify", "package", "publish"]
assert jobs["package"]["needs"] == "verify"
assert jobs["publish"]["needs"] == "package"
assert jobs["publish"]["if"] == "github.ref_type == 'tag'"
assert jobs["publish"]["permissions"] == {"contents": "write"}
assert "permissions" not in jobs["package"]
assert jobs["package"]["strategy"]["fail-fast"] is False
oses = [m["os"] for m in jobs["package"]["strategy"]["matrix"]["include"]]
assert oses == ["ubuntu-latest", "macos-latest", "windows-latest"], oses
up = [s for s in jobs["package"]["steps"] if str(s.get("uses","")).startswith("actions/upload-artifact")][0]
assert up["with"]["if-no-files-found"] == "error"
assert up["with"]["path"] == "dist/"
rel = [s for s in jobs["publish"]["steps"] if str(s.get("uses","")).startswith("softprops/")][0]
assert rel["with"]["draft"] is True
assert rel["with"]["fail_on_unmatched_files"] is True
print("release.yml: structure OK")
PY
```

Expected: `release.yml: structure OK`.

- [ ] **Step 3: Verify the staging logic against the artifacts this machine can build**

```bash
cargo build --release --locked
cargo packager --release --formats deb,appimage
bash -c '
  shopt -s nullglob
  want=2; found=(target/release/*.deb target/release/*.AppImage)
  printf "found: %s\n" "${found[@]}"
  [ "${#found[@]}" -eq "$want" ] || { echo "FAIL: got ${#found[@]}"; exit 1; }
  echo "staging logic OK"
'
```

Expected: two `found:` lines naming `chrome-host-app_<version>_amd64.deb` and
`chrome-host-app_<version>_x86_64.AppImage`, then `staging logic OK`.

(Requires `cargo install cargo-packager --version 0.11.8 --locked` once.)

- [ ] **Step 4: Verify the checksum step, including the space in the .dmg name**

```bash
bash -c '
  t=$(mktemp -d) && cd "$t" && mkdir dist
  echo a > "dist/chrome-host-app_0.1.0_amd64.deb"
  echo b > "dist/chrome-host-app_0.1.0_x86_64.AppImage"
  echo c > "dist/chrome-host-app_0.1.0_x64-setup.exe"
  echo d > "dist/Chrome Host App_0.1.0_universal.dmg"
  ( cd dist && sha256sum -- * ) > SHA256SUMS
  mv SHA256SUMS dist/
  [ "$(wc -l < dist/SHA256SUMS)" -eq 4 ] || { echo "FAIL: wrong entry count"; exit 1; }
  grep -q SHA256SUMS dist/SHA256SUMS && { echo "FAIL: self-listed"; exit 1; }
  ( cd dist && sha256sum -c SHA256SUMS ) && echo "checksum step OK"
  rm -rf "$t"
'
```

Expected: four `OK` lines including the space-containing `.dmg`, then
`checksum step OK`.

- [ ] **Step 5: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "feat: a release pipeline that cannot ship a release with a platform missing

The file this replaces has never run -- zero tags, zero releases -- and it had
five defects, two of which only became visible once cargo-packager was installed
and run rather than read about.

The .dmg is named from product-name instead of the binary name, so it is the one
artifact carrying spaces; and under the universal build it is the one artifact
not in target/release/, which the old upload glob would have missed without
failing. The result would have been a release that looks complete and has no
macOS installer in it. Three things now make that impossible: the staging step
asserts an exact artifact count per platform, publish needs every leg, and it
creates the release exactly once instead of racing three legs to do it.

macOS gets a universal binary, which forces an ad-hoc codesign -- lipo output
carries only a linker signature and recent macOS SIGKILLs those on Apple
Silicon, so the alternative is a .dmg that installs an app which dies on launch.
lipo -info and codesign --verify run in the job because nothing here can
reproduce that leg."
```

---

## Task 3: README §7 and §8

**Files:**
- Modify: `README.md` — §7 "Installers" (add two paragraphs), §8 "Releasing"
  (replace entirely)

**Interfaces:**
- Consumes: `scripts/release.sh <version>` from Task 1; the draft-release
  behaviour and artifact set from Task 2.
- Produces: nothing consumed by later tasks.

- [ ] **Step 1: Append to §7, after the "Code signing and notarization" list**

```markdown
The release workflow pins **cargo-packager 0.11.8** (`PACKAGER_VERSION` in
`.github/workflows/release.yml`). The installer filenames come from format
strings inside that tool and the workflow globs them, so pin the same version
locally if you want to reproduce what CI produces.
```

- [ ] **Step 2: Replace §8 entirely**

Replace everything from `## 8. Releasing` up to (not including) `## 9. Git service`:

```markdown
## 8. Releasing

`Cargo.toml` is the authority on the version. Cut a release with:

```bash
scripts/release.sh 1.2.0   # bumps [package].version, syncs Cargo.lock,
                           # commits, and creates the v1.2.0 tag
git push origin main
git push origin v1.2.0     # this is what starts the release
```

`scripts/release.sh` deliberately does not push. It refuses a dirty tree, a
version that is not semver, the version you are already on, and a tag that
already exists.

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
right-click → **Open**. Real signing and notarization remain a per-app TODO (§7).
```

- [ ] **Step 3: Verify the documented commands match what shipped**

```bash
python3 - <<'PY'
import pathlib, re
readme = pathlib.Path("README.md").read_text()
wf = pathlib.Path(".github/workflows/release.yml").read_text()
sh = pathlib.Path("scripts/release.sh").read_text()

assert "scripts/release.sh 1.2.0" in readme
assert "sha256sum -c SHA256SUMS --ignore-missing" in readme
assert "cargo-packager 0.11.8" in readme
# The version the README quotes must be the version the workflow pins.
pin = re.search(r'PACKAGER_VERSION:\s*"([^"]+)"', wf).group(1)
assert f"cargo-packager {pin}" in readme, f"README does not match pin {pin}"
# The error text the README quotes must be the text the workflow emits.
assert "::error::tag $GITHUB_REF_NAME != crate version $crate" in wf
assert "!= crate version" in readme
# The README promises release.sh does not push, so no line may actually run it.
# (It appears inside an echoed hint, which is why this checks executable lines.)
pushes = [l for l in sh.splitlines() if l.strip().startswith("git push")]
assert not pushes, f"release.sh pushes: {pushes}"
assert "does not push" in readme
print("README matches the implementation")
PY
```

Expected: `README matches the implementation`.

- [ ] **Step 4: Confirm no other README section contradicts the new §8**

```bash
grep -n "cargo packager\|release.yml\|installers\|Releasing" README.md
```

Expected: §7's local `cargo packager --release` instructions (unchanged, still
correct for local builds) and the new §8. Nothing describing the old
attach-per-leg behaviour.

- [ ] **Step 5: Commit**

```bash
git add README.md
git commit -m "docs: releasing, described as the pipeline now behaves

The old section described a workflow that had never run: it said each runner
attaches its installers to the release for the tag, which was both the shape
that got replaced and the shape that could leave a platform missing without
failing.

What a reader needs and did not have: that the version lives in Cargo.toml and
the tag is checked against it, that releases arrive as drafts they publish
themselves, that SHA256SUMS exists and how to check it, that a dry run is
available without cutting a tag, and that the macOS app is ad-hoc signed so the
first launch needs right-click -> Open rather than looking broken."
```

---

## Task 4: Verify on real runners

This is the only test Task 2 can have. Nothing on this machine builds a `.dmg` or
an NSIS installer, so the NSIS and dmg filenames in the spec's §2 table are read
from cargo-packager's source and are not yet confirmed. Run 1 confirms them.

**Files:** none — this task pushes and observes.

**Interfaces:**
- Consumes: Tasks 1–3, committed.
- Produces: the spec's §2 table promoted from *from source* to *measured*; §5.4's
  build-side reasoning confirmed.

- [ ] **Step 1: Push, so `workflow_dispatch` becomes available**

```bash
git push origin main
```

`workflow_dispatch` only appears in the Actions tab once the workflow is on the
default branch, so the dry run cannot gate its own arrival. Confirm CI is green
first — Task 1 added a step to `ci.yml`:

```bash
gh run list --branch main --limit 3
```

- [ ] **Step 2: Run 1 — the dry run**

```bash
gh workflow run Release --ref main
sleep 30 && gh run list --workflow=Release --limit 1
```

Then watch it and read the log:

```bash
gh run watch "$(gh run list --workflow=Release --limit 1 --json databaseId -q '.[0].databaseId')"
```

Expected: `verify` prints `dry run on main — skipping the version gate`; all
three `package` legs green; `publish` **skipped**.

Read out of the log and record:
- each leg's `found:` lines — the four real filenames, to check against the
  spec's §2 table
- the macOS `lipo -info` line — must report both `x86_64` and `arm64`
- the macOS `codesign --verify` line — must pass

If any filename differs from §2, amend the spec's table **and** the staging
globs before continuing.

- [ ] **Step 3: Run 2 — the negative test**

A gate that has never gone red is not known to work.

```bash
git tag -a v9.9.9 -m "gate test"
git push origin v9.9.9
```

Expected: `verify` fails in about thirty seconds with
`::error::tag v9.9.9 != crate version 0.1.0`, and `package` never starts.

```bash
gh run list --workflow=Release --limit 1
git push origin :refs/tags/v9.9.9 && git tag -d v9.9.9
```

- [ ] **Step 4: Run 3 — the positive test**

`Cargo.toml` is already at `0.1.0`, so no bump is needed and the gate passes
as-is.

```bash
git tag -a v0.1.0 -m "pipeline test"
git push origin v0.1.0
```

Expected: `verify` passes, three legs green, one **draft** release containing
exactly five files:

```bash
gh release view v0.1.0 --json isDraft,assets -q '{draft: .isDraft, assets: [.assets[].name]}'
```

- `chrome-host-app_0.1.0_amd64.deb`
- `chrome-host-app_0.1.0_x86_64.AppImage`
- `Chrome Host App_0.1.0_universal.dmg`
- `chrome-host-app_0.1.0_x64-setup.exe`
- `SHA256SUMS`

- [ ] **Step 5: Verify the checksums round-trip from a real download**

```bash
t=$(mktemp -d) && cd "$t"
gh release download v0.1.0 --repo "$(gh repo view --json nameWithOwner -q .nameWithOwner)"
sha256sum -c SHA256SUMS
cd - && rm -rf "$t"
```

Expected: four `OK` lines, including the space-containing `.dmg`.

- [ ] **Step 6: Clean up**

```bash
gh release delete v0.1.0 --yes
git push origin :refs/tags/v0.1.0 && git tag -d v0.1.0
git tag -l          # expect: empty
gh release list     # expect: empty
```

- [ ] **Step 7: Record what the runners measured**

Amend the spec's §2 table, changing the NSIS and dmg rows from **from source** to
**measured**, quoting the `found:` lines from run 1. Add a short "measured on the
runners" note to §9 recording the three run outcomes. If run 1 contradicted any
prediction, say so plainly rather than quietly correcting the table — that record
is the point of the provenance column.

```bash
git add docs/superpowers/specs/2026-08-07-release-pipeline-design.md
git commit -m "docs: the runners measured what the spec had only read from source"
```

---

## Self-Review

**Spec coverage.** §3 → Task 2 Step 1 (triggers, permissions, env). §4 → Task 2
`verify`. §5 and §5.1–5.5 → Task 2 `package` plus Steps 3–4. §6 → Task 2
`publish` plus Step 4. §7 → Task 1 (all seven requirements: dirty tree, semver,
same-version, existing tag, table-scoped edit, lockfile in the same commit, no
push — each has a matching assertion in `test-release.sh`). §8 → Task 3. §9 →
Task 4, all three runs plus cleanup. §10 is out-of-scope and needs no task.

**One addition beyond the spec.** The spec's §7 does not mention a test for
`release.sh`, and `cargo test` will never exercise a shell script. Task 1 adds
`scripts/test-release.sh` and one `ci.yml` step so the guards cannot rot. This
follows the standing spec-defect policy — fix it, amend the docs, keep going —
and Task 1 Step 8's commit message records why. Amend the spec's §7 to mention
the test when Task 1 lands.

**Placeholders.** None. Every code step carries the full file or the exact block
to insert; no step says "similar to Task N".

**Naming consistency.** `PACKAGER_VERSION` is used identically in Task 2's
workflow, Task 2 Step 2's assertion, and Task 3's README check. `dist/` is the
staging directory in both `package` and `publish`. `found` and `want` appear only
inside the staging step. The tag form is `v<version>` everywhere — `release.sh`
creates it, `verify` compares against it, Task 4 pushes and deletes it.

**Ordering.** Task 4 Step 1 must follow Tasks 1–3 because `workflow_dispatch`
requires the file on the default branch. Task 4 Step 4 must follow Step 3 so the
gate is proven to fail before it is trusted to pass.
