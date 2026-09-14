#!/usr/bin/env bash
#
# Exact merge-tree execution primitive retained from the SH-396 poller.
#
# SH-521 retires the public every-open-PR sweep. The daemon's store-backed
# `verifying` queue selects exactly one linked candidate and calls
# `verify-pr.sh`; that runner invokes this file only through the private
# `--speculative-run` entry below. The entry owns private merge objects and
# per-worktree Git administration, detached checkout, gate-command
# environment, restoration, and signal cleanup. The private administration
# keeps its synthetic HEAD out of the shared worktree ref namespace while its
# `commondir` preserves the shared receipt store. `tests/merge_gate.rs`
# provokes that boundary against real Git.
#
# A bare invocation fails with migration guidance after the private entry.
# The legacy polling body was retained unreachable for one compatibility
# cycle and removed by SH-654, which ships this file inside the binary: dead
# code that still reached `scripts/browser-status.sh` and
# `scripts/merge-preflight.sh` through the checkout was the only thing in the
# bundle that did.

set -uo pipefail

die() {
    printf 'merge-watch: %s\n' "$1" >&2
    exit 1
}

note() {
    printf 'merge-watch: %s\n' "$1" >&2
}

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" \
    || die "cannot resolve the production script directory"
root="$(git rev-parse --show-toplevel 2>/dev/null)" || die "not inside a git worktree"
cd "$root" || die "cannot enter $root"

common_dir="$(cd "$(git rev-parse --git-common-dir)" && pwd)" \
    || die "cannot resolve the shared git directory"
source_objects="$(cd "$common_dir/objects" && pwd -P)" \
    || die "cannot resolve the shared object directory"

# The real-Git core of an uncertified candidate, separated from the GitHub
# polling shell for the same reason land-pr.sh exposes --certified-run: tests
# can provoke object ownership, checkout, command failure, and signals without
# pretending a gh shim proves anything about GitHub. This is a private
# self-entry point, not an additional user-facing command.
if [ "${1:-}" = "--speculative-run" ]; then
    [ "$#" -ge 7 ] && [ "${6:-}" = "--" ] \
        || die "private usage: merge-watch.sh --speculative-run <expected-tree> <base-ref> <head-ref> <poller-worktree> -- <command...>"
    if ! python3 "$script_dir/verifier-owner.py" held "$common_dir" "$5"; then
        exec bash "$script_dir/machine-lock.sh" gate -- \
            python3 "$script_dir/verifier-owner.py" run "$common_dir" "$5" -- \
            bash "$script_dir/merge-watch.sh" "$@"
    fi
    expected_tree="$2"
    base="$3"
    head="$4"
    poller_wt="$5"
    shift 6

    base_commit="$(git rev-parse --verify "$base^{commit}" 2>/dev/null)" \
        || die "could not resolve speculative base $base to a commit"
    head_commit="$(git rev-parse --verify "$head^{commit}" 2>/dev/null)" \
        || die "could not resolve speculative head $head to a commit"
    # A concurrent fetch may advance either ref while the gate runs. The
    # private checkout must restore exactly the shared index we started with.
    base="$base_commit"
    head="$head_commit"

    python3 "$script_dir/verifier-worktree.py" ensure "$common_dir" "$poller_wt" "$base" \
        || die "could not restore the shared poller worktree lifecycle at $poller_wt"
    [ -e "$poller_wt/.git" ] \
        || die "the speculative poller worktree at $poller_wt has no .git entry"
    # Legacy interrupted runs can leave a missing private HEAD object. A
    # repair is safe only if the index and files already equal our base.
    poller_index_base=HEAD
    if ! git -C "$poller_wt" cat-file -e 'HEAD^{commit}' 2>/dev/null; then
        poller_index_base="$base"
    fi
    git -C "$poller_wt" diff --quiet \
        && git -C "$poller_wt" diff --cached --quiet "$poller_index_base" -- \
        || die "could not restore the shared poller worktree: tracked state is modified or unreadable at $poller_wt; preserving it for recovery"
    git -C "$poller_wt" checkout -q --detach "$base" \
        || die "could not restore the shared poller worktree to $base"
    shared_git_dir="$(cd "$(git -C "$poller_wt" rev-parse --absolute-git-dir)" && pwd -P)" \
        || die "cannot resolve the poller worktree's shared Git directory"
    case "$shared_git_dir/" in
    "$common_dir/worktrees/"*) ;;
    *) die "the speculative poller at $poller_wt is not a linked worktree of $common_dir" ;;
    esac

    lease_root="$common_dir/storyhook"
    mkdir -p "$lease_root" || die "could not create private merge state at $lease_root"
    lease_root="$(cd "$lease_root" && pwd -P)" \
        || die "cannot resolve the private merge-state directory"
    lease_prefix="$lease_root/merge-watch-objects."
    lease=""
    objects=""
    private_git=""
    poller_gitlink="$poller_wt/.git"
    original_gitlink=""
    private_gitlink=""
    gitlink_tmp=""
    child=""
    lease_registered=0
    inherited_alternates="${GIT_ALTERNATE_OBJECT_DIRECTORIES:-}"

    lease_is_valid() {
        candidate="$1"
        case "$candidate" in
        "$lease_prefix"*) ;;
        *) return 1 ;;
        esac
        suffix="${candidate#"$lease_prefix"}"
        [ -n "$suffix" ] || return 1
        case "$suffix" in
        */*) return 1 ;;
        esac
        [ -d "$candidate" ] && [ ! -L "$candidate" ] || return 1
        physical="$(cd "$candidate" && pwd -P)" || return 1
        [ "$physical" = "$candidate" ]
    }

    git_private() {
        GIT_OBJECT_DIRECTORY="$objects" \
            GIT_ALTERNATE_OBJECT_DIRECTORIES="$source_objects" \
            git "$@"
    }

    cleanup() {
        if [ "$lease_registered" -eq 1 ]; then
            python3 "$script_dir/verifier-worktree.py" recover "$common_dir" "$poller_wt" \
                || { note "could not restore owned state; preserving $lease"; return 1; }
            lease_registered=0
            lease=""
        elif [ -n "$lease" ] && lease_is_valid "$lease"; then
            rm -rf "$lease" || return 1
            lease=""
        fi
        if [ -n "$gitlink_tmp" ]; then
            rm -f "$gitlink_tmp" || return 1
            gitlink_tmp=""
        fi
    }

    on_signal() {
        signal="$1"
        if [ -n "$child" ]; then
            kill -s "$signal" "$child" 2>/dev/null || true
            wait "$child" 2>/dev/null || true
            child=""
        fi
        trap - "$signal" EXIT
        cleanup || true
        kill -s "$signal" $$
    }

    trap cleanup EXIT
    trap 'on_signal HUP' HUP
    trap 'on_signal INT' INT
    trap 'on_signal TERM' TERM

    created_lease="$(python3 "$script_dir/verifier-worktree.py" allocate "$common_dir" "$poller_wt" "$base")" \
        || die "could not create private merge object storage"
    lease="$created_lease"
    lease_registered=1
    canonical_lease="$(cd "$created_lease" && pwd -P)" \
        || die "could not resolve private merge object storage"
    lease="$canonical_lease"
    lease_is_valid "$lease" \
        || die "created object storage outside the expected private location"
    objects="$lease/objects"
    private_git="$lease/.git"
    original_gitlink="$lease/original-gitlink"
    private_gitlink="$lease/private-gitlink"
    mkdir -p "$objects" "$private_git" \
        || die "could not initialize private merge state at $lease"
    cp "$poller_gitlink" "$original_gitlink" \
        || die "could not preserve the poller worktree's Git link"
    {
        printf '%s\n' "$base_commit" > "$private_git/HEAD"
        printf '%s\n' "$common_dir" > "$private_git/commondir"
        printf '%s\n' "$poller_gitlink" > "$private_git/gitdir"
        printf 'gitdir: %s\n' "$private_git" > "$private_gitlink"
    } || die "could not initialize private per-worktree Git administration"
    GIT_DIR="$private_git" \
        GIT_WORK_TREE="$poller_wt" \
        GIT_OBJECT_DIRECTORY="$objects" \
        GIT_ALTERNATE_OBJECT_DIRECTORIES="$source_objects" \
        git read-tree "$base" \
        || die "could not initialize the private poller index"

    candidate_tree="$(bash "$script_dir/merge-preflight.sh" \
        --object-dir "$objects" "$base" "$head")"
    preflight_status=$?
    case "$preflight_status" in
    0 | 1) ;;
    2) exit 2 ;;
    *) die "merge preflight failed with status $preflight_status" ;;
    esac
    [ "$candidate_tree" = "$expected_tree" ] \
        || die "predicted tree changed from $expected_tree to ${candidate_tree:-<none>} before the speculative run"

    merge_commit="$(git_private -C "$poller_wt" commit-tree "$candidate_tree" \
        -p "$base" -p "$head" -m "merge-watch: speculative merge")" \
        || die "could not build the speculative merge commit"
    gitlink_tmp="$(mktemp "$poller_wt/.git.merge-watch.XXXXXX")" \
        || die "could not stage the private poller Git link"
    cp "$private_gitlink" "$gitlink_tmp" \
        || die "could not stage the private poller Git link"
    python3 "$script_dir/verifier-worktree.py" register "$common_dir" "$poller_wt" "$lease" "$base" \
        || die "could not record speculative recovery intent"
    lease_registered=1
    mv -f "$gitlink_tmp" "$poller_gitlink" \
        || die "could not activate private per-worktree Git administration"
    gitlink_tmp=""
    git_private -C "$poller_wt" checkout -q --detach "$merge_commit" \
        || die "could not check out the speculative merge"

    candidate_alternates="$objects"
    if [ -n "$inherited_alternates" ]; then
        candidate_alternates="$candidate_alternates:$inherited_alternates"
    fi

    # `wait` is explicit so signal traps run immediately instead of being
    # deferred until a long gate command exits. fd 3 preserves stdin for a
    # command that needs it, matching machine-lock.sh's transparent wrapper.
    exec 3<&0
    (
        trap - HUP INT TERM
        cd "$poller_wt" || exit 1
        # WHAT THIS LIST MUST NEVER CONTAIN: `STORYHOOK_MACHINE_LOCKS`, and
        # `STORYHOOK_GATE_PROGRESS_ACTIVITY_PATH`. `verify-pr.sh` runs this
        # whole script under `machine-lock.sh gate`; the gate command is
        # `make test` (unless the project's `[verify] gate` says otherwise,
        # SH-649), which reaches `run-tests.sh`, which re-execs itself
        # under `machine-lock.sh gate` -- and that inner take is reentrant
        # ONLY because the name list travels in the environment
        # (`machine-lock.sh`, "REENTRANCY"). A holder is judged by liveness,
        # never a clock, and the outer holder is this process's own ancestor,
        # provably alive: strip the variable and every verification on the
        # machine deadlocks against itself. The activity path is what the
        # inner lock reports its wait through. Nothing fenced this before
        # SH-646; `docs/spec/verification-workflow.md`, "The locks", is the
        # statement of record. The names below are scrubbed because each
        # would make the gate answer about the wrong object store, the wrong
        # daemon store, the wrong project, or with a token it must not hold.
        # The gate chooses when it certifies; the bundle supplies the writer.
        # Set it here rather than trusting an inherited path from another run.
        exec env -u GIT_OBJECT_DIRECTORY \
            -u STORYHOOK_GATE_RESULT_FILE \
            -u STORYHOOK_STORE_PATH \
            -u STORYHOOK_PROJECT \
            -u GH_TOKEN \
            -u GITHUB_TOKEN \
            GIT_ALTERNATE_OBJECT_DIRECTORIES="$candidate_alternates" \
            STORYHOOK_GATE_RECEIPT="$script_dir/tree-receipt.sh" \
            python3 "$script_dir/verifier-owner.py" gate "$common_dir" "$poller_wt" -- "$@"
    ) <&3 &
    child=$!
    exec 3<&-
    wait "$child"
    command_status=$?
    child=""

    cleanup \
        || die "the gate command finished, but the poller worktree could not be restored; recovery evidence was preserved"
    trap - EXIT HUP INT TERM
    # The completion record means "the gate command ran to its own exit and the
    # poller was restored". A child that died by signal (status 128+n, the
    # supervisor's encoding of a signal death) did not complete: it judged
    # nothing, so it earns no record and the caller's missing-record branch
    # classifies it as infrastructure. Publishing 143 here is how SH-692's
    # terminated gate could have been read as a red — 143 agreed with 143 —
    # and it is the one line a cancellation can turn into a verdict.
    if [ -n "${STORYHOOK_GATE_RESULT_FILE:-}" ] && [ "$command_status" -lt 128 ]; then
        printf '%s\n' "$command_status" > "$STORYHOOK_GATE_RESULT_FILE" \
            || die "could not publish the completed gate command status"
    fi
    exit "$command_status"
fi

die "the broad open-PR sweep is retired; move one linked story to verifying and the StoryHook daemon will invoke scripts/verify-pr.sh. The private --speculative-run core remains for exact merge-tree verification."
