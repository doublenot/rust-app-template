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

# The only guard against releasing a version twice, and deliberately the only
# one. There is no "refuses the version you are already on" check: a crate's
# first release has nothing to bump, and refusing it made this script unusable
# for the one release every fork of this template cuts first. The invariant is
# that any given version can be tagged exactly once, and this enforces it.
! git rev-parse -q --verify "refs/tags/v$new" >/dev/null || die "tag v$new already exists"

# First release: the manifest already carries the version being tagged, so there
# is nothing to edit and nothing to commit. Tag the current commit and stop --
# committing anyway would put an empty "chore: release" on every first release.
if [ "$cur" = "$new" ]; then
  git tag -a "v$new" -m "v$new"
  echo "tagged v$new (manifest was already at $new, so no bump was needed)"
  echo "push when ready:  git push origin v$new"
  exit 0
fi

# Scoped to the [package] table, not substituted across the file. This manifest
# already carries a second `version = "0.1.0"` under [package.metadata.packager],
# and a dependency could be pinned to the same string -- both are
# indistinguishable from the real one to sed.
#
# The lookahead is line-anchored (^\[) rather than a plain [^\[]*, which would
# stop at the '[' opening an array value such as `keywords = [...]`. Measured:
# the truncating form never corrupts the manifest -- it captures a short table,
# finds no version key, and refuses -- but it refuses on manifests this form
# handles, so the anchored one is what keeps working as [package] grows.
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
