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
#
# Two fixtures earn their place, and their ORDER is part of the test:
#
#   - `keywords` is an array value, placed BEFORE `version` on purpose. A
#     table-scoping regex that stops at the first '[' character captures a table
#     truncated at the array, finds no version key in it, and refuses -- so with
#     the array first this fixture makes that mistake fail the run. Put the array
#     after `version` and both forms pass, which is why it is where it is.
#   - [package.metadata.packager] carries a SECOND `version` key holding the same
#     string as the real one, which a naive substitution rewrites.
fixture() {
  local d
  d=$(mktemp -d)
  mkdir -p "$d/src"
  cat > "$d/Cargo.toml" <<'EOF'
[package]
name = "fixture"
edition = "2021"
keywords = ["release", "test"]
version = "0.1.0"

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

# --- the first release ----------------------------------------------------
# A crate's first release has nothing to bump: the manifest already carries the
# version being tagged. Refusing that made this script unusable for the one
# release every fork of this template cuts first, so the same-version case is
# allowed and simply skips the bump. The invariant is that any given version can
# be tagged exactly once, and the "tag already exists" guard is what enforces it.
d=$(fixture)
before=$(git -C "$d" rev-parse HEAD)
( cd "$d" && "$release_sh" 0.1.0 >/dev/null 2>&1 )
check "cuts a first release at the current version" 0 $?

assert "the first-release tag exists"  git -C "$d" rev-parse -q --verify refs/tags/v0.1.0
assert "the manifest is left alone"    grep -q '^version = "0.1.0"' "$d/Cargo.toml"

# The bump branch commits; this branch must not, or every first release would
# carry an empty "chore: release" commit.
if [ "$(git -C "$d" rev-parse HEAD)" = "$before" ]; then
  ok "no empty commit is created"
else
  not_ok "no empty commit is created"
fi

if [ -z "$(git -C "$d" status --porcelain)" ]; then
  ok "the tree is clean after a first release"
else
  not_ok "the tree is clean after a first release"
fi

# What the dropped same-version guard was really protecting: releasing a version
# twice. That is now the tag guard's job, so prove it does it.
( cd "$d" && "$release_sh" 0.1.0 >/dev/null 2>&1 )
check "refuses to cut the same version twice" 1 $?
rm -rf "$d"

echo
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
