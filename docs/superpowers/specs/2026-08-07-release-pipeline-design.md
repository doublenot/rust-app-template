# Release pipeline for `chrome-host-app` — design

Status: approved, ready to plan.

Every filename and path below carries its provenance: **measured** means it was
produced on this machine by `cargo-packager 0.11.8` and observed; **from source**
means it was read out of the cargo-packager tree and has not yet been produced;
**unverified** means neither, and the verification plan in §9 is what closes it.
That distinction is the point of this document. The previous subsystem shipped a
Windows fix whose stated mechanism turned out to be wrong even though the fix
worked, and it was caught only by measuring on a real runner. This pipeline
touches three operating systems, two of which cannot be exercised here.

---

## 0. Provenance

Decided by the user across two rounds on 2026-08-06/07. These are constraints,
not proposals; a reviewer who disagrees with one is disagreeing with a decision,
not with this document.

| # | decision |
|---|---|
| 1 | **Scope is GitHub Releases only.** Tag → build on three OSes → attach installers and checksums. Not package registries (brew/winget/apt), not containers. |
| 2 | **`Cargo.toml` is the authority on version.** The workflow's first job asserts the tag equals the crate version and hard-fails on mismatch. A helper script makes bump + lockfile + commit + tag one command. |
| 3 | **macOS ships one universal `.dmg`.** Both targets are built on the arm64 runner and `lipo`'d together. |
| 4 | **Releases are created as drafts.** The user publishes manually, so a broken build is never public. |
| 5 | **Verification cuts real throwaway tags** against this repo, then deletes the tags and the draft. |
| 6 | **Structure: `verify` → `package` (matrix) → `publish`.** Artifacts round-trip through workflow storage; exactly one job creates the release. |

Decision 6 was chosen over two alternatives. *Fan-out-and-attach* (the shape the
current file has) lets all three legs call the release action, which races on
creation and — the real defect — leaves a convincing partial draft when one leg
fails. *A reusable `build.yml` plus a thin `release.yml`* buys first-class dry
runs at the cost of two files and `workflow_call` plumbing; §3 gets the same dry
run from one conditional.

---

## 1. What exists today, and why it has never worked

`.github/workflows/release.yml` exists, is documented in README §7–8, and has
**never run**: the repository has zero tags and zero releases, and its Actions
history is CI-only. It is the same shape of problem as the git service's first
Windows run — written, documented, never exercised.

It has five defects, each of which this design closes:

| # | defect | closed by |
|---|---|---|
| 1 | A `v1.0.0` tag would build and ship artifacts named `0.1.0`, because nothing compares the tag to `Cargo.toml`. | §4 |
| 2 | `macos-latest` is arm64, so the `.dmg` is Apple-Silicon-only. Intel Macs get nothing. | §5 |
| 3 | `cargo install cargo-packager` recompiles the tool from source on every leg of every run, unpinned. | §5 |
| 4 | The upload globs were never verified. `target/release/*.dmg` in particular matches nothing under decision 3 (§2). | §5, §6 |
| 5 | No checksums, and `contents: write` is granted to all three build legs. | §6 |

Defect 4 is the dangerous one. A glob that matches nothing is not an error in
`softprops/action-gh-release@v2` by default, so the failure mode is a release
that looks complete and is quietly missing a platform.

---

## 2. Established facts

These four filenames are the contract between §5 and §6. Getting them wrong is
defect 4 all over again.

| format | filename | directory | provenance |
|---|---|---|---|
| deb | `chrome-host-app_0.1.0_amd64.deb` | `target/release/` | **measured** — 6.7 MB, Linux x86_64 |
| AppImage | `chrome-host-app_0.1.0_x86_64.AppImage` | `target/release/` | **measured** — 16.0 MB, Linux x86_64 |
| NSIS | `chrome-host-app_0.1.0_x64-setup.exe` | `target/release/` | **from source** — `format!("{}_{}_{}-setup.exe", main_binary_name, config.version, arch)` |
| dmg | `Chrome Host App_0.1.0_universal.dmg` | `target/universal-apple-darwin/release/` | **from source** — `format!("{}_{}_{}", config.product_name, config.version, arch)` |

The `.dmg` is the outlier twice over, and both differences are load-bearing:

- It is the **only** format named from `product-name` rather than the binary
  name, so it is the only one containing **spaces**. Any shell that handles it
  must quote.
- Under decision 3 it is the only one **not** in `target/release/`.
  `cargo-packager` computes its directory as `target/<triple>/<profile>` when a
  target triple is supplied and `target/<profile>` when it is not (from source:
  `if let Some(target_triple) = … { cargo_out_dir.push(target_triple); }
  cargo_out_dir.push(profile);`). `target_arch()` maps `universal-apple-darwin`
  to the literal string `"universal"` (from source).

Three further facts, all established rather than assumed:

**`cargo-packager` publishes no prebuilt binaries.** Its latest GitHub release,
`@crabnebula/packager-v0.11.8`, carries zero assets (measured via `gh release
view`), so `cargo-binstall` and `taiki-e/install-action` cannot help. An npm
distribution with per-platform napi binaries exists but sits at **0.11.2**
against the crate's **0.11.8** (measured via the npm registry API); a lagging
second toolchain is not worth the minutes it saves. §5 caches a pinned
`cargo install` instead.

**`cargo-packager` never ad-hoc signs.** Its macOS bundle step is guarded by
`if let Some(identity) = config.macos().and_then(|m| m.signing_identity…)`, with
no fallback (from source). Without a configured identity it skips signing
entirely.

**`--formats dmg` builds the `.app` in the same pass.** The Dmg arm checks
whether App was already packaged and, if not, packages it inline before calling
`dmg::package` (from source). There is therefore no seam between the two in
which to sign the bundle — §5 signs the binary beforehand instead.

---

## 3. The job graph

```
on: push tags v*  |  workflow_dispatch

  ┌──────────┐
  │  verify  │   tag == crate version, ~10s
  └────┬─────┘   no-ops under workflow_dispatch
       │
  ┌────┴────────────────────────┐
  │      package (matrix)       │   fail-fast: false
  │  ubuntu     macos    windows│
  │  deb,       dmg      nsis   │
  │  appimage  (universal)      │
  └────┬────────────────────────┘
       │  actions/upload-artifact
  ┌────┴─────┐
  │ publish  │   if: github.ref_type == 'tag'
  └──────────┘   download-all → SHA256SUMS → ONE draft release
```

```yaml
name: Release
on:
  push:
    tags: ["v*"]
  workflow_dispatch:      # dry run: builds all three legs, touches no release

permissions:
  contents: read          # only `publish` is granted write, in §6

env:
  PACKAGER_VERSION: "0.11.8"
```

The current file grants `contents: write` at the top level, so all three build
legs hold a token that can rewrite the repository. Only the job that creates the
release needs it.

`workflow_dispatch` is what makes the dry run possible: it builds every leg and
produces every artifact while `publish` is skipped by its `if`. One caveat
governs the rollout order — **`workflow_dispatch` appears in the Actions tab
only once the workflow exists on the default branch**, so the file has to land on
`main` before a dry run can be started. The dry run cannot gate its own arrival.

The gate is on `ref_type`, not on the event, and that is deliberate: dispatching
the workflow against a *tag* rather than a branch runs `publish` too. That is the
supported way to re-cut a release whose upload failed, without inventing a new
tag. Dispatching against a branch — the ordinary case — can never touch a release.

---

## 4. `verify`

```yaml
jobs:
  verify:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Tag must match the crate version
        run: |
          if [ "$GITHUB_REF_TYPE" != tag ]; then
            echo "dry run on $GITHUB_REF_NAME — skipping the version gate"; exit 0
          fi
          crate=$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[0].version')
          [ "v$crate" = "$GITHUB_REF_NAME" ] || {
            echo "::error::tag $GITHUB_REF_NAME != crate version $crate"; exit 1; }
          echo "tag $GITHUB_REF_NAME matches crate version $crate"
```

`--no-deps` performs no resolution, needs no network and no system libraries: it
reads `Cargo.toml` and exits. The whole job costs about ten seconds and saves
roughly thirty runner-minutes on the mismatch it exists to catch (defect 1).

`jq` is used here and again in §5.3. It is preinstalled on both images that need
it — this job pins itself to `ubuntu-latest`, and the macOS runner image lists
`jq 1.8.2` (checked against `actions/runner-images`). No Windows step uses it.

The `GITHUB_REF_TYPE` branch is the dry-run path. Exiting 0 satisfies
`needs: verify`, so the matrix proceeds; `publish` is gated separately.

---

## 5. `package`

```yaml
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

      - name: Cache cargo-packager
        id: packager-cache
        uses: actions/cache@v4
        with:
          path: ~/.cargo/bin/cargo-packager*
          key: cargo-packager-${{ env.PACKAGER_VERSION }}-${{ runner.os }}-${{ runner.arch }}
      - if: steps.packager-cache.outputs.cache-hit != 'true'
        run: cargo install cargo-packager --version ${{ env.PACKAGER_VERSION }} --locked

      - name: Build and package
        if: runner.os != 'macOS'
        shell: bash
        run: |
          cargo build --release --locked
          cargo packager --release --formats ${{ matrix.formats }} --verbose

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
          codesign --force --sign - "target/universal-apple-darwin/release/$bin"
          lipo -info "target/universal-apple-darwin/release/$bin"
          codesign --verify --verbose "target/universal-apple-darwin/release/$bin"
          cargo packager --release --target universal-apple-darwin --formats dmg --verbose

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
```

### 5.1 The staging step, and why the globs are not uploaded directly

This step closes defect 4, and it exists for two independent reasons.

**Flattening.** `upload-artifact` roots an artifact at the **least common
ancestor of its search paths**. Uploading `target/release/*.deb` alongside
`target/universal-apple-darwin/release/*.dmg` therefore roots at `target/` and
preserves the nesting beneath it, so after §6's `merge-multiple` the `dist/`
directory would contain *directories* rather than files — breaking both
`sha256sum -- *` and `files: dist/*`. Staging into a flat `dist/` first makes
`path: dist/` a single search path with an unambiguous root.

**A counted assertion beats an undocumented one.** `if-no-files-found: error`
is documented only for the case where the whole path specification matches
nothing; its behaviour when *some* of several patterns match is unspecified.
Asserting the exact count this platform is expected to produce does not depend
on that, and it is strictly stronger: it catches a leg that emits one file where
two were expected, which no `if-no-files-found` setting can. The upload's own
`if-no-files-found: error` is kept as a backstop.

`shopt -s nullglob` is required, not decorative: without it an unmatched glob
expands to its own literal text, so `found` would be non-empty and the count
would pass while `cp` failed on a nonexistent path. `shell: bash` gives Git Bash
on the Windows runner, which supports it.

### 5.2 Pinning `PACKAGER_VERSION`

Pinning is not only about the cache key. The output filenames in §2 are this
workflow's contract with §6, and they come from format strings inside
cargo-packager. An unpinned upgrade could rename them, and — absent §5.1 — that
would surface as a silently incomplete release rather than a failure.

### 5.3 The binary name is derived, not hardcoded

`cargo metadata --no-deps … .packages[0].default_run` returns `chrome-host-app`
(measured on this machine). The package has two bin targets — `gen_icons` is the
other — so `default_run` is the disambiguator, and it is the same key
`cargo build` itself uses. This repository is a **template**; hardcoding the
binary name would leave a landmine for the first person who forks it and renames
the crate.

### 5.4 Ad-hoc signing is mandatory, not cosmetic

This was the design's one genuine surprise, and it reverses the position taken
before the pause.

`lipo -create` output carries only the minimal signature the Apple linker
applies, and recent macOS rejects linker-signed-only binaries on Apple Silicon
with `SIGKILL`. Combined with §2's finding that cargo-packager never ad-hoc
signs, an unsigned universal build would produce a `.dmg` that installs an app
which **dies on launch** — not a Gatekeeper prompt, a crash. `codesign --force
--sign -` is therefore load-bearing.

Because `--formats dmg` builds the `.app` in the same pass (§2), the signature
must go on the binary *before* packaging copies it into `Contents/MacOS/`.

**Epistemic status: unverified.** The SIGKILL behaviour is drawn from public
reports, not from a machine available here, and no Mac in this project has run
the resulting app. §9 puts `lipo -info` and `codesign --verify` in the run log so
the build side is proven; confirming the app actually *launches* needs a human
with a Mac and is listed in §10 as out of scope for the pipeline itself.

### 5.5 Cost

The macOS leg compiles the whole dependency graph twice — vendored libgit2,
libssh2 and OpenSSL included — so it runs roughly twice as long as the others.
`fail-fast: false` is kept so one platform's failure still leaves the other two
legs' logs and artifacts available for diagnosis.

---

## 6. `publish`

```yaml
  publish:
    needs: package
    if: github.ref_type == 'tag'
    runs-on: ubuntu-latest
    permissions:
      contents: write
    steps:
      - uses: actions/download-artifact@v4
        with:
          path: dist
          merge-multiple: true

      - name: Checksums
        run: |
          # Computed in a subshell so the redirect lands outside dist/. Do not
          # "simplify" this to a find/xargs pipeline and do not assume dist/ is
          # clean -- both make SHA256SUMS list itself. Table below.
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

`needs: package` is what makes a partial release impossible. `fail-fast: false`
still lets you see every leg, but any one of them failing means `publish` never
runs at all, so there is no half-populated draft to mistake for a finished one.

The checksum step runs in a **subshell** so the redirect lands outside `dist/`.
The reason is narrower than it looks, and was established by measurement rather
than by reading the shell grammar — an earlier draft of this section justified it
incorrectly. With four files staged in `dist/`:

| form | entries | self-listed? |
|---|---|---|
| `( cd dist && sha256sum -- * > SHA256SUMS )` | 4 | no |
| `( cd dist && find . -type f -print0 \| xargs -0 sha256sum > SHA256SUMS )` | 5 | **yes** |
| glob form, but `SHA256SUMS` already exists | 5 | **yes** |

The plain glob form is *safe on a clean directory*: the shell completes pathname
expansion before it performs the redirect, so `SHA256SUMS` does not yet exist
when `*` is expanded. It stops being safe in exactly two ways — `find` runs as a
separate process *after* the redirect has created the file, and a `SHA256SUMS`
left over from any earlier step or rerun is matched by `*` like anything else.
The subshell removes the dependency on both, which is worth one line for a step
whose failure mode is a checksum manifest that silently checksums itself.

`sha256sum -- *` handles the space in `Chrome Host App_0.1.0_universal.dmg`:
the shell passes it as a single argv entry, and GNU coreutils escapes only
backslashes and newlines. `sha256sum -c SHA256SUMS` round-trips all four names,
verified locally.

`fail_on_unmatched_files: true` is §5.1's counterpart on the publish side.

---

## 7. `scripts/release.sh`

Decision 2's second half. One command for bump + lockfile refresh + commit + tag.

```
scripts/release.sh 1.2.0
```

Required behaviour:

| # | it must | because |
|---|---|---|
| 1 | refuse a dirty working tree | the tag would capture unrelated work |
| 2 | refuse a non-semver argument | the tag drives artifact names |
| 3 | refuse a version equal to the current one | a no-op bump usually means a typo |
| 4 | refuse when `refs/tags/v<version>` already exists | tags are what the pipeline keys on |
| 5 | edit `version` **only inside the `[package]` table** | a naive substitution would rewrite a dependency that happens to carry the same string |
| 6 | refresh `Cargo.lock` in the same commit | a lockfile disagreeing with the manifest fails `--locked` on all three runners |
| 7 | create the commit and the annotated tag, and **not push** | pushing the tag starts a release; that stays a deliberate act |

Requirement 5 is the one worth restating: `version = "0.1.0"` appears in
`[package]`, and a dependency pinned to the same string would be indistinguishable
to `sed`. The edit must be scoped to the `[package]` table.

Requirement 7 keeps the script safe to run and re-run. `git push origin v1.2.0`
is the separate, deliberate step that starts a release.

---

## 8. Documentation changes

**README §7 (Installers)** — add the pinned packager version, and note that the
macOS build is universal via `lipo` and ad-hoc signed.

**README §8 (Releasing)** — currently four sentences describing the fan-out shape
this design replaces. It is rewritten to cover: the version gate and what its
error looks like; `scripts/release.sh`; the `workflow_dispatch` dry run; that
releases arrive as **drafts** awaiting a manual publish; `SHA256SUMS` together
with the `sha256sum -c` invocation a user actually runs; and that the macOS app
is ad-hoc signed, so Gatekeeper reports "unidentified developer" and right-click
→ Open is the documented workaround.

Code signing and notarization remain a per-app TODO, as §7 already states. Nothing
in this design changes that; ad-hoc signing is what makes the app *run*, not what
makes it *trusted*.

---

## 9. Verification plan

Decision 5 authorizes real tags against this repository. Three runs, in order,
after the workflow has landed on `main` (§3):

| # | run | expected |
|---|---|---|
| 1 | **Dry run** — `workflow_dispatch` from the Actions tab | all three legs green; `publish` skipped; artifacts present. Read the macOS log for `lipo -info` reporting `x86_64 arm64` and `codesign --verify` passing, and read each leg's `found:` lines (§5) to check the four filenames in §2 against reality — this is what promotes NSIS and dmg from *from source* to *measured*. |
| 2 | **Negative test** — push tag `v9.9.9` | `verify` fails in about thirty seconds and the matrix never starts. |
| 3 | **Positive test** — push tag `v0.1.0` | a draft release carrying four installers plus `SHA256SUMS`. |

Run 2 is not optional. A gate that has never gone red is not known to work — the
same mutation discipline applied to every fix in the git service. Run 3 needs no
version bump because `Cargo.toml` is already at `0.1.0`, so the gate passes as-is.

Cleanup afterwards: delete the draft release, then both tags locally and on the
remote.

---

## 10. Out of scope

- **Package registries** — brew, winget, apt repositories, containers (decision 1).
- **Real code signing and notarization** — unchanged per-app TODO (§8). Ad-hoc
  signing makes the app run; it does not make it trusted.
- **Confirming the macOS app launches.** §5.4's reasoning is closed on the build
  side by §9 run 1, but a human with a Mac must open the `.dmg` and launch the
  app to close it on the runtime side. This is the same class of gap as the two
  dialog smoke checks in the git service.
- **Linux arm64 and Windows arm64.** GitHub's runner matrix can supply both; no
  demand has been stated, and each adds a leg.
- **Automatic publishing.** Drafts are deliberate (decision 4).
