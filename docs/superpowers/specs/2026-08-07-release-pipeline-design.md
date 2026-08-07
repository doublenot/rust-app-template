# Release pipeline for `hitch` — design

Status: approved, ready to plan.

Every filename and path below carries its provenance: **measured** means it was
produced and observed — on this machine or on a named CI run; **from source**
means it was read out of the cargo-packager tree and has not yet been produced;
**unverified** means neither, and the verification plan in §9 is what closes it.
That distinction is the point of this document. The previous subsystem shipped a
Windows fix whose stated mechanism turned out to be wrong even though the fix
worked, and it was caught only by measuring on a real runner. This pipeline
touches three operating systems, two of which cannot be exercised here.

> **All four filenames in §2 are now measured.** The NSIS and dmg rows were
> *from source* when this document was written and were confirmed by the dry runs
> recorded in §9. The predictions were correct — including the `.dmg` carrying
> spaces — but two things the design did *not* predict came out of the same runs
> and are recorded as defect 6 (§5.6) and in §9.

### 0.1 The app was renamed after this document was written

Everything below said `chrome-host-app` / `Chrome Host App` when it was written.
The crate, binary, `product-name` and identifier were renamed to **`hitch`** /
**`Hitch`** / `com.example.hitch` after v0.1.1, because the old name read as a
relative of Google's discontinued Chrome Apps platform.

The rename was applied throughout, with one deliberate exception: **quoted
observations keep the names that were actually observed.** §5.7 records a real
failure whose whole substance is a filename containing a space, and §9.1 records
CI runs that emitted `chrome-host-app_*` artifacts. Rewriting those would have
made this document claim measurements that never happened. Everywhere else —
including §2's contract table — carries the current name, because those describe
what the pipeline produces now.

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
| 6 | Every installer ships **both** bin targets, so `gen_icons` — a build-time placeholder-icon generator — lands in the user's `/usr/bin`. | §5.6 |

Defect 4 is the dangerous one. A glob that matches nothing is not an error in
`softprops/action-gh-release@v2` by default, so the failure mode is a release
that looks complete and is quietly missing a platform.

> **Defect 6 was found by the first dry run, not by this design.** It is listed
> here because it belongs with the others, but nothing in the original analysis
> caught it: `dpkg -c` on a locally built `.deb` shows `usr/bin/gen_icons`
> alongside `usr/bin/hitch`. See §5.6.

---

## 2. Established facts

These four filenames are the contract between §5 and §6. Getting them wrong is
defect 4 all over again.

| format | filename | directory | provenance |
|---|---|---|---|
| deb | `hitch_0.1.0_amd64.deb` | `target/release/` | **measured** — 6.7 MB, Linux x86_64 |
| AppImage | `hitch_0.1.0_x86_64.AppImage` | `target/release/` | **measured** — 16.0 MB, Linux x86_64 |
| NSIS | `hitch_0.1.0_x64-setup.exe` | `target/release/` | **measured** — windows-latest, run 31183489259 |
| dmg | `Hitch_0.1.0_universal.dmg` | `target/universal-apple-darwin/release/` | **measured** — macos-latest, run 31184474049 |

These filenames were measured under the old crate name and are shown here with
the new one (§0.1); the runs cited emitted `chrome-host-app_*`. The *shape* is
what was established, and the rename does not change it — except that `Hitch`
has no space where `Chrome Host App` did, which makes §5.7 dormant rather than
unnecessary.

The `.dmg` is the outlier twice over, and both differences are load-bearing:

- It is the **only** format named from `product-name` rather than the binary
  name, so it is the only one that can contain **spaces** — as it did until the
  rename. Any shell that handles it must quote, and §5.7 covers what quoting
  alone does not.
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
        # Block style, not `- { os: ..., formats: deb,appimage }`. In a YAML flow
        # mapping the comma is an entry separator, so the compact form parses as
        # formats: "deb" plus a junk key `appimage: null` -- the Linux leg then
        # packages the deb, exits 0, and silently produces no AppImage.
        include:
          - os: ubuntu-latest
            formats: deb,appimage
          - os: macos-latest
            formats: dmg
          - os: windows-latest
            formats: nsis
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

`cargo metadata --no-deps … .packages[0].default_run` returns `hitch`
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

**This section was wrong, and a Mac proved it.** See §5.8. Signing the binary is
necessary and is *not* sufficient: Gatekeeper validates the bundle, not the
executable inside it. The paragraph above stands as far as it goes; what it
missed is everything the bundle needs.

### 5.8 Signing the binary is not signing the bundle

v0.1.2 was installed on a real Mac and macOS offered to **move it to the Trash
as damaged**. That is the outcome §2.5 of `docs/macos.md` listed as the worst of
three, and §5.4 had predicted an unidentified-developer warning instead.

The reasoning failed at a join. §5.4 correctly established that the `lipo`'d
binary must be ad-hoc signed, and §2 correctly established that cargo-packager
signs nothing without a configured identity. Both were true, and the conclusion
drawn from them — sign the binary before packaging copies it in — quietly
assumed that a validly signed executable makes a valid *app*. It does not.
Gatekeeper validates the bundle: `Hitch.app` had no `_CodeSignature` at all, and
a quarantined download of an unsigned bundle is precisely what "damaged" means.

Nothing in CI could have caught this. The job runs `codesign --verify` **on the
binary**, and it passes — the binary was never the problem.

The fix is to let cargo-packager do the signing it already knows how to do:

```toml
[package.metadata.packager.macos]
signing-identity = "-"
```

Measured on the runner (run `31196542098`), that one line makes it:

```
Codesigning .../Hitch.app
Running Command `"xattr" "-cr" ".../Hitch.app"`
Codesigning .../Hitch.app/Contents/MacOS/hitch
Codesigning .../Hitch.app with identity "-"
Codesigning .../Hitch_0.1.2_universal.dmg
```

— the executable, the bundle, *and* the disk image, with extended attributes
cleared first. The risk that held this on a branch did not materialise:
cargo-packager passes `--timestamp`, ad-hoc signatures cannot be timestamped,
and `codesign` accepted the combination anyway rather than failing the leg.

**Still unverified:** that this turns "damaged" into an ordinary
unidentified-developer block. Ad-hoc signing is not Developer ID and does not
notarize anything, so macOS will still refuse the first launch — the claim being
made is only that it refuses it *for the right reason*, recoverably. That needs
the same Mac again, against a build carrying this fix.

**The general lesson, which is why §5.4 is left standing rather than rewritten:**
two separately-correct findings were combined into a conclusion neither
supported, and it survived a green pipeline, a verified checksum, and a released
artifact. It took a person opening the app.

### 5.5 Cost

The macOS leg compiles the whole dependency graph twice — vendored libgit2,
libssh2 and OpenSSL included — so it runs roughly twice as long as the others.
`fail-fast: false` is kept so one platform's failure still leaves the other two
legs' logs and artifacts available for diagnosis.

### 5.6 `binaries` must be declared, or every bin target gets packaged

Added after the first dry run. `[package.metadata.packager]` now carries:

```toml
binaries = [{ path = "hitch", main = true }]
```

Without it cargo-packager packages **every** bin target the crate declares, and
this crate has two. That is defect 6 on Linux and Windows — `gen_icons` was
shipping inside the installers — and a hard failure on macOS, where only the
main binary is `lipo`'d into `target/universal-apple-darwin/release/` and the
copy of the other one fails the whole job:

```
ERROR cargo_packager::cli: Failed to copy file from
  .../target/universal-apple-darwin/release/gen_icons to ...
```

This is the third time in this design that the crate's *second* bin target has
mattered — §5.3 derives the binary name from `default_run` for the same reason.
A single-binary crate would never have surfaced any of it.

### 5.7 Spaces are stripped during staging

Added after the first *published* release, and **written when `product-name` was
still `Chrome Host App`** — the names below are quoted as they were observed, not
translated into the current one (see §0.1).

GitHub rewrites spaces in a release asset's filename. The `.dmg` that
cargo-packager produced as `Chrome Host App_0.1.0_universal.dmg` was uploaded as
`Chrome.Host.App_0.1.0_universal.dmg`, while `SHA256SUMS` recorded the name
cargo-packager had produced. Downloading the release and running the documented
check gave:

```
sha256sum: 'Chrome Host App_0.1.0_universal.dmg': No such file or directory
Chrome Host App_0.1.0_universal.dmg: FAILED open or read
```

and with the `--ignore-missing` that README §8 recommends it is worse than a
failure — the command reports success while silently never verifying the one
file that needed it.

So the staging loop copies each artifact as `${base// /-}`. Spaces are gone
before `SHA256SUMS` is computed, the uploaded name is the name in the manifest,
and `sha256sum -c` round-trips. Only the file is renamed; `product-name` and the
app's display name are untouched.

**This is now dormant**, because `Hitch` has no space in it. It stays anyway:
`product-name` is among the first things a fork changes, and a two-word name
would walk straight back into it — silently, since the pipeline goes green either
way and only a download reveals the problem.

It was also the second defect in this design traceable to one root cause: the
`.dmg` being the only artifact named from `product-name`. §2 flagged the spaces
and concluded "any shell that handles it must quote", which was right and
insufficient — every shell did quote correctly, and the thing that renamed the
file was not a shell.

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

`sha256sum -- *` handles the space in `Hitch_0.1.0_universal.dmg`:
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
| 3 | accept a version equal to the current one, tagging **without** a bump or a commit | a crate's first release has nothing to bump (§7.1) |
| 4 | refuse when `refs/tags/v<version>` already exists | tags are what the pipeline keys on |
| 5 | edit `version` **only inside the `[package]` table** | a naive substitution would rewrite a dependency that happens to carry the same string |
| 6 | refresh `Cargo.lock` in the same commit | a lockfile disagreeing with the manifest fails `--locked` on all three runners |
| 7 | create the commit and the annotated tag, and **not push** | pushing the tag starts a release; that stays a deliberate act |

Requirement 5 is the one worth restating: `version = "0.1.0"` appears in
`[package]`, and a dependency pinned to the same string would be indistinguishable
to `sed`. The edit must be scoped to the `[package]` table.

Requirement 7 keeps the script safe to run and re-run. `git push origin v1.2.0`
is the separate, deliberate step that starts a release.

### 7.1 Requirement 3 was originally the opposite, and was wrong

This section first specified *"refuse a version equal to the current one — a
no-op bump usually means a typo."* That guard was implemented, tested, and then
made the script unusable the first time anyone tried to use it for its actual
purpose: cutting `v0.1.0` on a manifest sitting at `0.1.0` produced

```
release: already at 0.1.0
```

which is the **first release of every fork of this template**. The real v0.1.0
had to be tagged by hand.

The guard was protecting against releasing a version twice, and requirement 4
already does that — a duplicate tag is refused. So requirement 3 is inverted:
when `new == cur`, the script tags the current commit and stops, skipping the
manifest edit and the commit. The invariant is now stated positively: **any
given version can be tagged exactly once.**

Note what is *not* guarded as a result: at `0.2.0`, `release.sh 0.1.0` will
happily bump the version *down*. That is a different typo from the one
requirement 3 was aimed at, it has never been guarded here, and it is left
alone rather than folded in silently.

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

### 9.1 What the runners actually measured

All three runs were performed on 2026-08-07. It took **five** runs, not three,
because the first dry run and the first published release each found a defect.

| run | id | outcome |
|---|---|---|
| dry run | `31183489259` | **failed.** Linux: `expected 2 artifact(s) on Linux, got 1` — the matrix was written as a YAML flow mapping, so `formats: deb,appimage` parsed as `formats: "deb"` plus a junk key. macOS: `Failed to copy … /gen_icons` → defect 6, §5.6. Windows passed, promoting NSIS to *measured*. |
| dry run | `31184474049` | **passed.** All three legs green, `publish` skipped, all four filenames confirmed — promoting dmg to *measured*. |
| negative | — | **passed.** `verify` failed with `tag v9.9.9 != crate version 0.1.0`; `package` and `publish` both skipped, so the matrix never started. |
| positive | `31185495123` | **published, then failed verification.** Draft correct, five assets — but the downloaded `.dmg` did not match `SHA256SUMS`, because GitHub had renamed it. → §5.7. |
| positive | `31185885528` | **passed.** `sha256sum -c SHA256SUMS` exits 0 on all four downloaded installers with no `--ignore-missing`. |

The macOS build-side reasoning in §5.4 is confirmed. Quoted as the runners
emitted it, which was before the rename (§0.1) — the binary is `hitch` now:

```
Architectures in the fat file: …/chrome-host-app are: x86_64 arm64
…/chrome-host-app: replacing existing signature
…/chrome-host-app: valid on disk
…/chrome-host-app: satisfies its Designated Requirement
```

`replacing existing signature` is the detail worth keeping: it confirms the
`lipo` output really did carry the linker signature §5.4 predicted, which is what
made the ad-hoc `codesign` necessary rather than decorative.

**Scorecard.** The design predicted all four filenames correctly, including that
the `.dmg` would carry spaces. It did not predict that a second bin target would
be packaged into every installer, nor that GitHub would rewrite an asset name.
Both were found by running the thing — the first by an assertion written for
exactly that failure, the second only by downloading a real release and checking
it. Neither was reachable by reading.

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
