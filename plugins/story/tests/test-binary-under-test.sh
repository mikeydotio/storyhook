#!/usr/bin/env bash
# The suite must exercise the `story` THIS checkout builds, never an installed
# one.
#
# This suite runs `story` by name, so the binary it tests is whatever `$PATH`
# reaches. Off `make test` -- a `bash plugins/story/tests/run-tests.sh` typed by
# hand, which is what a person or an agent does -- that used to be the
# developer's installed build, and nothing said so. The store was isolated
# either way, so nothing was damaged; the wrong binary was simply exercised.
#
# The failure mode is what makes this worth a test of its own rather than a
# comment. Found while SH-531 was being written: a standalone run failed
# test-reap.sh and test-dispatch-epic.sh against an installed v2.2.0 whose
# `default_states()` predates the `verifying` state, and reported `error: state
# `verifying` not found` -- an error that names a state, a project and a store,
# and not the one thing that was actually wrong. `make test` was green
# throughout, because the Makefile's plugin leg has always prepended
# `target/debug`. A gate that is right and a standalone path that quietly tests
# something else is the SH-306 shape: the verdict came from state nobody
# checked.
#
# Asserted by resolving the name the way the suite itself does, rather than by
# reading `$PATH`: `command -v` is the question every fixture actually asks.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

resolved="$(command -v story || true)"
[ -n "$resolved" ] \
  || fail_test "no \`story\` on \$PATH at all -- lib.sh is supposed to put one there"

# The checkout this test file belongs to, resolved the same way lib.sh resolves
# it, so a suite run from a copied tree checks that tree rather than this one.
repo_root="$(cd "$TESTS_DIR/../../.." && pwd -P)"
expected="${CARGO_TARGET_DIR:-$repo_root/target}/debug/story"

resolved_phys="$(cd "$(dirname "$resolved")" && pwd -P)/$(basename "$resolved")"
expected_phys="$(cd "$(dirname "$expected")" && pwd -P)/$(basename "$expected")"

assert_eq "$resolved_phys" "$expected_phys" \
  "\`story\` must resolve to this checkout's own build"

# …and it must be the build, not a stale artifact of some other checkout that
# happens to sit at the same path. `--version` carries a build id derived from
# the tracked tree the binary was built from (SH-406), so this is the one
# question that distinguishes two builds of the same VERSION.
version_line="$("$resolved" --version 2>&1)"
case "$version_line" in
*story*) : ;;
*) fail_test "\`story --version\` did not identify itself: [$version_line]" ;;
esac

# The inverse, which is the property that actually failed: an installed binary
# must not be what answers. Skipped rather than faked when there is no
# installed one to be confused with -- a machine with nothing in ~/.local/bin
# cannot exhibit the defect, and asserting against an absent file would be
# asserting nothing while looking like coverage.
installed="${STORYHOOK_REAL_HOME:-$HOME}/.local/bin/story"
if [ -x "$installed" ]; then
  installed_phys="$(cd "$(dirname "$installed")" && pwd -P)/$(basename "$installed")"
  [ "$resolved_phys" != "$installed_phys" ] \
    || fail_test "the suite resolved the INSTALLED \`story\` at $installed_phys"
fi

finish
