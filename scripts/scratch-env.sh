#!/usr/bin/env bash
#
# A disposable storyhook: this checkout's binary, a throwaway store, a daemon
# that dies with your shell.
#
# WHY THIS EXISTS. `./target/debug/story list`, typed in a worktree, resolves
# the developer's REAL store and the real daemon on port 3456. `is_test_build`
# does not stop it -- that sentinel is the `fault-injection` feature, which
# `cargo test` sets and `cargo build` does not -- and this repository carries a
# committed `.storyhook.toml`, so the project it resolves is the one storyhook
# uses to track itself. Until this script there was no way to exercise a change
# by hand except `make install`, which replaces the binary on $PATH for
# everything else on the machine.
#
# There was never anything wrong with the primitives: `story store new`, and the
# parameters `story help test-environment` names, have always been enough. What
# was missing was one command that uses them, so that "try it out" does not
# begin with four steps a person can get three-quarters right.
#
#   bash scripts/scratch-env.sh                 # a shell in a scratch env
#   bash scripts/scratch-env.sh --test-build    # ...running a TEST build
#   bash scripts/scratch-env.sh -- story list   # one command, then exit
#   eval "$(bash scripts/scratch-env.sh --print)"   # this shell, no subshell
#   bash scripts/scratch-env.sh --fresh         # empty it first
#
#   make scratch            # the same, ARGS="..." to pass options
#   make scratch-clean      # delete every scratch environment
#
# THE ENVIRONMENT IS THE TEST SUITE'S OWN. `scripts/test-env.sh` is what both
# use, so a scratch environment is isolated in exactly the way, and to exactly
# the degree, `make test` is -- and `story help test-environment` documents both
# at once. That is the whole design: a person exercising a change by hand should
# not be running under a weaker contract than the gate.
#
# $HOME IS YOURS UNLESS YOU SAY OTHERWISE. The store, the daemon, its port, its
# logs and its backups are all keyed off the other parameters, so the real $HOME
# is reached for exactly one thing storyhook does -- `story daemon install`,
# which writes a launchd agent -- and keeping it buys you your shell's rc file,
# your git identity and your ssh keys. `--isolate-home` makes it hermetic when
# that is what you want.
#
# THE ROOT PERSISTS, unlike every other harness root in this repository, which
# `trap ... EXIT`s itself away. Coming back to a store you set up is the point.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=test-env.sh
. "$repo_root/scripts/test-env.sh"

# Every scratch environment lives under here, so `--clean` has one place to
# look and a stale one is easy to find. `/private/tmp` for the reason
# `storyhook_isolate` requires it: disposable, and not Spotlight-indexed.
readonly SCRATCH_BASE="/private/tmp/storyhook-scratch"

name="default"
profile="debug"
binary=""
fresh=0
print_only=0
isolate_home=""
command_args=()

usage() {
    cat >&2 <<'USAGE'
usage: scratch-env.sh [--name NAME] [--release | --test-build] [--binary PATH]
                      [--fresh] [--isolate-home] [--print] [-- COMMAND...]

  --name NAME      which scratch environment (default: "default"). Separate
                   names are separate stores and separate daemons.
  --release        exercise the optimized build.
  --test-build     exercise a build carrying the `fault-injection` feature --
                   the one `cargo test` produces, and the only one whose store
                   crash points can be armed by hand.
  --binary PATH    use this binary and build nothing.
  --fresh          delete this environment before starting.
  --isolate-home   redirect $HOME too. Hermetic, at the cost of your shell's
                   rc file, your git identity and your ssh keys.
  --print          print the environment as shell and exit, for `eval`.
  -- COMMAND...    run one command in the environment instead of a shell.
USAGE
}

while [ "$#" -gt 0 ]; do
    case "$1" in
    --name)
        [ "$#" -ge 2 ] || { echo "scratch-env.sh: --name needs a value" >&2; exit 2; }
        name="$2"
        shift 2
        ;;
    --release)
        profile="release"
        shift
        ;;
    --test-build)
        profile="test"
        shift
        ;;
    --binary)
        [ "$#" -ge 2 ] || { echo "scratch-env.sh: --binary needs a path" >&2; exit 2; }
        binary="$2"
        shift 2
        ;;
    --fresh)
        fresh=1
        shift
        ;;
    --isolate-home)
        isolate_home="--home"
        shift
        ;;
    --print)
        print_only=1
        shift
        ;;
    -h | --help)
        usage
        exit 0
        ;;
    --)
        shift
        command_args=("$@")
        break
        ;;
    *)
        # An argument that lands nowhere is refused, never dropped.
        echo "scratch-env.sh: unexpected argument [$1]" >&2
        usage
        exit 2
        ;;
    esac
done

# A name becomes a directory, so it may not climb out of the base.
case "$name" in
"" | *[/\\]* | .. | .)
    echo "scratch-env.sh: [$name] is not a usable environment name -- it becomes a" >&2
    echo "  directory under $SCRATCH_BASE, so it may not contain a path separator." >&2
    exit 2
    ;;
esac

root="$SCRATCH_BASE/$name"

if [ "$fresh" -eq 1 ]; then
    # Bounded by construction: `$root` is always under $SCRATCH_BASE, which the
    # name check above is what guarantees.
    rm -rf "$root"
fi
mkdir -p "$root"

# --- the binary ------------------------------------------------------------
#
# Built before the environment is applied, deliberately: `cargo` wants the real
# $HOME for its registry and its cache, and under `--isolate-home` it would not
# have one.
if [ -z "$binary" ]; then
    case "$profile" in
    debug)
        (cd "$repo_root" && cargo build) >&2
        binary="$repo_root/target/debug/story"
        ;;
    release)
        (cd "$repo_root" && cargo build --release) >&2
        binary="$repo_root/target/release/story"
        ;;
    test)
        # The same feature `storyhook-test-support` turns on as a
        # dev-dependency, which is what makes `cargo test`'s binary a test
        # build. Note this overwrites `target/debug/story`, so the next
        # `cargo build` (and `make test`, which runs one) puts the ordinary
        # binary back.
        (cd "$repo_root" && cargo build --features fault-injection) >&2
        binary="$repo_root/target/debug/story"
        ;;
    esac
fi

if [ ! -x "$binary" ]; then
    echo "scratch-env.sh: [$binary] is not an executable file" >&2
    exit 1
fi
binary_dir="$(cd "$(dirname "$binary")" && pwd)"

# --- the environment -------------------------------------------------------

if [ "$print_only" -eq 1 ]; then
    if [ -n "$isolate_home" ]; then
        storyhook_isolate_print "$isolate_home" "$root"
    else
        storyhook_isolate_print "$root"
    fi
    printf "export PATH='%s':\"\$PATH\"\n" "$binary_dir"
    exit 0
fi

if [ -n "$isolate_home" ]; then
    storyhook_isolate "$isolate_home" "$root"
else
    storyhook_isolate "$root"
fi
export PATH="$binary_dir:$PATH"

# WHICH BUILD AND WHICH STORE, both stated rather than inferred. `--version`
# carries a build id derived from the tracked tree it was built from, so two
# installs of one VERSION are told apart here -- which is the whole reason a
# scratch environment is safe to trust.
{
    echo "storyhook scratch environment [$name]"
    echo "  binary  $binary"
    echo "          $("$binary" --version 2>&1 | head -1)"
    echo "  store   $STORYHOOK_STORE_PATH"
    echo "  daemon  ephemeral port, dies with pid $STORYHOOK_PARENT_PID"
    if [ -n "$isolate_home" ]; then
        echo "  home    $HOME (isolated)"
    else
        echo "  home    $HOME (yours -- --isolate-home to redirect it)"
    fi
    echo "  delete  rm -rf $root"
} >&2

if [ "${#command_args[@]}" -gt 0 ]; then
    exec "${command_args[@]}"
fi

echo "  exit this shell to end the daemon" >&2
exec "${SHELL:-/bin/sh}"
