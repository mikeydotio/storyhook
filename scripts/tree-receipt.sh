#!/usr/bin/env bash
#
# Project-independent tree certification — SH-665, extracted from SH-306's
# gate-receipt.sh. That wrapper still owns StoryHook's push-hook enrollment;
# this core never reads or writes hook policy.
#
# A project gate calls preflight BEFORE its first leg and postlude AFTER all
# legs pass. The verifier supplies this executable's absolute path through
# STORYHOOK_GATE_RECEIPT. Merely returning zero from a gate certifies nothing.
#
# Receipts retain the shared <git-common-dir>/storyhook/gate-receipts format
# and changed < gate < full ordering. Only gate/full can certify a merge.
# Preflight state and owned temporary objects stay private to the worktree;
# the tracked tree must match at both ends. Atomic publication prevents an
# interrupted postlude from publishing a partial receipt. See test-tiers.md
# and verification-workflow.md for the contract and its threat model.

set -uo pipefail

phase="${1:-}"

die() {
    printf 'tree-receipt: %s\n' "$1" >&2
    exit 1
}

note() {
    printf 'tree-receipt: %s\n' "$1" >&2
}

root="$(git rev-parse --show-toplevel 2>/dev/null)" \
    || die "not inside a git worktree"
cd "$root" || die "cannot enter $root"

# `--git-dir` is this worktree's PRIVATE directory (a linked worktree gets its
# own), which is where transient per-run state belongs: two worktrees running
# the suite at once must not read each other's preflight.
git_dir="$(cd "$(git rev-parse --git-dir)" && pwd)" \
    || die "cannot resolve this worktree's git directory"

# `--git-common-dir` is shared by every worktree, which is where receipts
# belong: the key is content, so two worktrees holding byte-identical trees
# genuinely did test the same thing.
common_dir="$(cd "$(git rev-parse --git-common-dir)" && pwd)" \
    || die "cannot resolve the shared git directory"

receipts="$common_dir/storyhook/gate-receipts"
preflight_state="$git_dir/storyhook-gate-preflight"
source_objects="$(cd "$common_dir/objects" && pwd -P)" \
    || die "cannot resolve the shared object directory"
inherited_alternates="${GIT_ALTERNATE_OBJECT_DIRECTORIES:-}"
source_alternates="$source_objects"
if [ -n "$inherited_alternates" ]; then
    source_alternates="$source_alternates:$inherited_alternates"
fi
lease_prefix="$git_dir/storyhook-gate-objects."

lease=""
lease_identity_value=""
lease_marker_value=""
cleanup_lease=0
cleanup_state=0
state_tmp=""
receipt_tmp=""

lease_path_is_scoped() {
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
}

lease_identity() {
    case "$(uname -s)" in
    Darwin) stat -f '%d:%i' "$1" 2>/dev/null ;;
    Linux) stat -c '%d:%i' -- "$1" 2>/dev/null ;;
    *) return 1 ;;
    esac
}

lease_is_directory() {
    lease_path_is_scoped "$1" && [ ! -L "$1" ] && [ -d "$1" ]
}

lease_marker_is_valid() {
    case "$1" in
    .storyhook-owner.*) ;;
    *) return 1 ;;
    esac
    marker_suffix="${1#.storyhook-owner.}"
    [ -n "$marker_suffix" ] || return 1
    case "$marker_suffix" in
    */*) return 1 ;;
    esac
}

lease_is_owned() {
    lease_is_directory "$1" || return 1
    actual_identity="$(lease_identity "$1")" || return 1
    [ "$actual_identity" = "$2" ] || return 1
    lease_marker_is_valid "$3" || return 1
    [ ! -L "$1/$3" ] && [ -f "$1/$3" ]
}

remove_lease() {
    # Recursive deletion is allowed only while the scoped path, physical
    # directory identity, and in-directory capability marker still name the
    # lease this caller created. A substituted entry is left in place;
    # removing an unproven object is less safe than leaking one bounded
    # worktree-private artifact.
    lease_is_owned "$1" "$2" "$3" || return 1
    rm -rf "$1"
}

cleanup() {
    if [ -n "$receipt_tmp" ]; then
        rm -f "$receipt_tmp"
        receipt_tmp=""
    fi
    if [ -n "$state_tmp" ]; then
        rm -f "$state_tmp"
        state_tmp=""
    fi
    if [ "$cleanup_state" -eq 1 ]; then
        rm -f "$preflight_state"
        cleanup_state=0
    fi
    if [ "$cleanup_lease" -eq 1 ] && [ -n "$lease" ]; then
        remove_lease "$lease" "$lease_identity_value" "$lease_marker_value" 2>/dev/null || true
        lease=""
        lease_identity_value=""
        lease_marker_value=""
        cleanup_lease=0
    fi
}

on_signal() {
    status="$1"
    trap - EXIT HUP INT TERM
    cleanup
    exit "$status"
}

trap cleanup EXIT
trap 'on_signal 129' HUP
trap 'on_signal 130' INT
trap 'on_signal 143' TERM

load_preflight_state() {
    [ -f "$preflight_state" ] || return 1
    state_body="$(cat "$preflight_state" 2>/dev/null)" || return 1
    state_tree="$(sed -n 's/^tree //p' "$preflight_state" 2>/dev/null)"
    state_lease="$(sed -n 's/^objects //p' "$preflight_state" 2>/dev/null)"
    state_identity="$(sed -n 's/^identity //p' "$preflight_state" 2>/dev/null)"
    state_marker="$(sed -n 's/^marker //p' "$preflight_state" 2>/dev/null)"
    expected_state="tree $state_tree
objects $state_lease
identity $state_identity
marker $state_marker"
    [ "$state_body" = "$expected_state" ] || return 1
    printf '%s\n' "$state_tree" | grep -Eq '^[0-9a-f]{40}([0-9a-f]{24})?$' \
        || return 1
    printf '%s\n' "$state_identity" | grep -Eq '^[0-9]+:[0-9]+$' \
        || return 1
    lease_is_owned "$state_lease" "$state_identity" "$state_marker"
}

git_with_lease() {
    lease_is_owned "$lease" "$lease_identity_value" "$lease_marker_value" || return 1
    GIT_OBJECT_DIRECTORY="$lease" \
        GIT_ALTERNATE_OBJECT_DIRECTORIES="$source_alternates" \
        git "$@" || return $?
    lease_is_owned "$lease" "$lease_identity_value" "$lease_marker_value"
}

# The tree object id of every TRACKED file as it currently stands in this
# worktree — HEAD's tree with any uncommitted edits to tracked files folded in.
# Untracked files are excluded on purpose: `target/`, `e2e/node_modules` and
# the suite's own scratch output are not what is being certified, and `git add
# -A` is forbidden here for the same reason.
#
# Extracted to `scripts/tracked-tree.sh` when SH-406 gave this primitive a
# second caller (`build.rs`, which stamps every build with this value).
# Invoked as a subprocess rather than sourced — see that file's header for the
# council verdict this follows and why: it turns both callers onto one
# process contract (stdout carries the oid, a nonzero exit means "no
# answer"), provable end to end the same way `tests/push_gate.rs` already
# proves this script, rather than an in-process sourcing relationship a test
# could only pin by static inspection.
tracked_tree() {
    if [ "$#" -eq 3 ]; then
        lease_is_owned "$1" "$2" "$3" || return 1
        tracked_tree_result="$("$(dirname "${BASH_SOURCE[0]}")/tracked-tree.sh" "$1")" \
            || return $?
        lease_is_owned "$1" "$2" "$3" || return 1
        printf '%s\n' "$tracked_tree_result"
    else
        "$(dirname "${BASH_SOURCE[0]}")/tracked-tree.sh"
    fi
}

case "$phase" in
preflight)
    # A killed or abandoned gate can leave the prior caller-owned object
    # lease behind. Reap it only after validating that the state names an
    # actual directory in this worktree's private git directory. Corrupt
    # state is removed but never followed to an arbitrary deletion target.
    if [ -e "$preflight_state" ]; then
        if load_preflight_state; then
            note "discarding an abandoned preflight for tree $state_tree"
            remove_lease "$state_lease" "$state_identity" "$state_marker" \
                || die "could not remove abandoned object storage"
            rm -f "$preflight_state" \
                || die "could not remove abandoned preflight state"
        else
            rm -f "$preflight_state" \
                || die "could not remove invalid preflight state"
            die "discarded invalid preflight state without deleting any object directory; run the gate again"
        fi
    fi
    cleanup_state=1

    lease="$(mktemp -d "$lease_prefix"XXXXXX)" \
        || die "could not create private object storage"
    cleanup_lease=1
    lease_is_directory "$lease" \
        || die "created object storage outside the expected worktree-private location"
    lease_identity_value="$(lease_identity "$lease")" \
        || die "could not identify private object storage"
    printf '%s\n' "$lease_identity_value" | grep -Eq '^[0-9]+:[0-9]+$' \
        || die "private object storage has an invalid physical identity"
    lease_marker_path="$(mktemp "$lease/.storyhook-owner.XXXXXX")" \
        || die "could not create private object storage ownership marker"
    lease_marker_value="${lease_marker_path##*/}"
    lease_is_owned "$lease" "$lease_identity_value" "$lease_marker_value" \
        || die "could not validate private object storage ownership"

    tree="$(tracked_tree "$lease" "$lease_identity_value" "$lease_marker_value")" \
        || die "could not resolve this worktree's tracked content"
    state_tmp="$preflight_state.tmp.$$"
    {
        printf 'tree %s\n' "$tree"
        printf 'objects %s\n' "$lease"
        printf 'identity %s\n' "$lease_identity_value"
        printf 'marker %s\n' "$lease_marker_value"
        true
    } > "$state_tmp" || die "could not stage the preflight state"
    mv -f "$state_tmp" "$preflight_state" \
        || die "could not record the preflight tree"
    state_tmp=""
    cleanup_lease=0
    cleanup_state=0
    ;;

postlude)
    [ -f "$preflight_state" ] \
        || die "no preflight was recorded — the gate must run \
'tree-receipt.sh preflight' before its first leg, or nothing certified is \
certified against anything"
    if ! load_preflight_state; then
        rm -f "$preflight_state" \
            || die "could not remove invalid preflight state"
        die "preflight state is invalid; no object directory was deleted and nothing was certified"
    fi
    before="$state_tree"
    lease="$state_lease"
    lease_identity_value="$state_identity"
    lease_marker_value="$state_marker"
    cleanup_lease=1
    cleanup_state=1

    tier="${2:-gate}"
    base_tree="${3:-}"
    case "$tier" in
    changed | gate | full) ;;
    *) die "unknown tier '$tier' -- postlude takes 'changed', 'gate' or 'full'" ;;
    esac
    if [ "$tier" = "changed" ]; then
        [ -n "$base_tree" ] \
            || die "postlude changed needs a base tree: 'tree-receipt.sh postlude changed <base-tree>'"
        base_receipt="$receipts/$base_tree"
        base_tier="$(sed -n 's/^tier //p' "$base_receipt" 2>/dev/null | head -n1)"
        base_tier="${base_tier:-gate}"
        [ -f "$base_receipt" ] && { [ "$base_tier" = "gate" ] || [ "$base_tier" = "full" ]; } \
            || die "base tree $base_tree carries no gate/full receipt of its own -- a \
changed receipt must name a tree that was itself fully certified, not another selective run"
    elif [ -n "$base_tree" ]; then
        die "only 'changed' takes a base tree; got tier '$tier' with base '$base_tree'"
    fi

    after="$(tracked_tree "$lease" "$lease_identity_value" "$lease_marker_value")" \
        || die "could not resolve this worktree's tracked content"

    # Mid-run drift. Nine minutes is long enough for an agent to edit tracked
    # files while the suite runs, and a receipt written blind at the end would
    # certify a mixture no single run ever tested end to end.
    if [ "$before" != "$after" ]; then
        changed_paths="$(git_with_lease diff --name-only "$before" "$after" 2>/dev/null)" \
            || die "could not compare the preflight tree with the current tracked content"
        note "tracked content changed while the suite was running, so this run \
certifies nothing. Changed:"
        printf '%s\n' "$changed_paths" | sed 's/^/  /' >&2
        die "run the gate again on the tree you intend to push"
    fi

    mkdir -p "$receipts" || die "could not create $receipts"

    # A receipt already on file for this exact tree at an EQUAL OR STRONGER
    # tier is a claim this run's own tier cannot improve on -- don't erase it.
    # `changed < gate < full` is the whole order; a `changed` run over a tree
    # that already carries `gate` must not "downgrade" it back to a partial
    # claim, and the pre-existing full-over-gate rule generalizes cleanly.
    tier_rank() {
        case "$1" in
        changed) printf '1\n' ;;
        gate) printf '2\n' ;;
        full) printf '3\n' ;;
        esac
    }
    if [ -f "$receipts/$after" ]; then
        existing_tier="$(sed -n 's/^tier //p' "$receipts/$after" 2>/dev/null | head -n1)"
        existing_tier="${existing_tier:-gate}"
        if [ "$(tier_rank "$tier")" -lt "$(tier_rank "$existing_tier")" ]; then
            note "tree $after already carries a $existing_tier receipt; a $tier-tier \
run does not downgrade it"
            exit 0
        fi
    fi

    lease_is_owned "$lease" "$lease_identity_value" "$lease_marker_value" \
        || die "private object storage changed before receipt publication"

    # Atomic: the filename IS the claim, so a half-written one would read as
    # valid. rename(2) within a directory is atomic; a SIGTERM mid-write — the
    # exact failure this story measured — leaves the temp file, not a receipt.
    receipt_tmp="$receipts/.tmp.$$"
    {
        printf 'tree %s\n' "$after"
        printf 'worktree %s\n' "$root"
        printf 'tier %s\n' "$tier"
        if [ "$tier" = "changed" ]; then
            printf 'base %s\n' "$base_tree"
        fi
        true
    } > "$receipt_tmp" || die "could not stage the receipt"
    mv -f "$receipt_tmp" "$receipts/$after" || die "could not publish the receipt"
    receipt_tmp=""
    ;;

*)
    die "usage: tree-receipt.sh preflight|postlude [gate|full]"
    ;;
esac
