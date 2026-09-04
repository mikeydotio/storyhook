#!/usr/bin/env bash
#
# `storyhook_isolate` — point this shell at a throwaway storyhook environment.
#
# THE SHELL RENDERING OF ONE DEFINITION. The parameters, and the reason each
# one exists, live in `src/env/test_environment.rs` and ship in the binary as
# `story help test-environment`. The table below is that table written in
# shell, and `tests/test_environment.rs` proves the two agree by running both
# and comparing the environments they actually produce — never by comparing
# their shapes, which is a check that passes while the two mean different
# things.
#
# WHY IT IS ONE FILE. This block used to be hand-copied into six harnesses, and
# they had already drifted: three carried a path guard and three did not, one
# used a sentinel pid, and none of them cleared the credential the Rust harness
# had been clearing since SH-153. A seventh copy would have been the seventh
# chance to get it wrong. Story data lives in one store per machine, so the cost
# of a harness that isolates four variables out of five is a suite writing into
# somebody's real tracker, silently.
#
# HOW TO USE IT:
#
#   . "$(dirname "$0")/test-env.sh"
#   storyhook_isolate "$root"                        # a wrapper around cargo/npm
#   storyhook_isolate --home "$root"                 # a wrapper around story/git
#   storyhook_isolate --parent-pid "$sentinel" "$root"
#   storyhook_isolate_print --home "$root"           # emit it instead of adopting it
#
# `--home` ALSO REDIRECTS $HOME, AND MOST CALLERS MUST NOT PASS IT. Other tools
# read $HOME too: on an ordinary machine $CARGO_HOME and $RUSTUP_HOME are unset,
# so a fake $HOME around `cargo test` costs cargo its registry and its build
# cache, and one around the browser suite costs playwright its downloaded
# browsers — silently, as a large slowdown rather than an error. Pass it only
# from a harness that runs nothing but `story` and `git`. A harness that cannot
# pass it is not thereby unisolated: `storyhook-test-support`'s `TestEnv`
# redirects $HOME on each `story` child instead, which is the level a wrapper
# around cargo cannot reach and a test binary can.
#
# `--parent-pid` hands ownership of the spawned daemons to some pid other than
# this shell's, for a caller that wants to end them earlier than it ends itself
# (`scripts/coverage-map.sh` kills a sentinel between test binaries). The
# default is `$$`, which is the ordinary case: the daemon dies with the run.
#
# A FAILED ISOLATION EXITS RATHER THAN RETURNS. A caller that could carry on
# past a refusal would carry on unisolated, which is the exact outcome the
# refusal exists to prevent, and in at least one harness there is no second
# guard behind it (`scripts/run-e2e.sh` runs a NON-test binary, so the
# environment is the only thing standing between it and a real store). This is
# not a status a caller may choose to ignore.

# The parameter table: NAME SCOPE DISPOSITION ARGUMENT.
#
#   SCOPE        any   — safe to export across a whole harness process tree
#                story — only on a storyhook process itself (see --home above)
#   DISPOSITION  root <tail>      a path beneath the environment root
#                literal <value>  a fixed value
#                ownpid -         the pid that owns the spawned daemons
#                clear -          removed; there is no harmless value
#
# One list, read by both functions below, in `test_environment.rs`'s own order
# so that a reader comparing the two files reads one list twice rather than two.
_storyhook_test_environment() {
    cat <<'TABLE'
HOME story root home
XDG_DATA_HOME any root home/.local/share
XDG_CONFIG_HOME any root home/.config
XDG_STATE_HOME any root home/.local/state
STORYHOOK_DATA_DIR any root home/.local/share/storyhook
STORYHOOK_STORE_PATH any root home/.local/share/storyhook/store.db
STORYHOOK_DAEMON_ADDR any literal 127.0.0.1:0
STORYHOOK_PARENT_PID any ownpid -
STORYHOOK_GITHUB_TOKEN any clear -
STORYHOOK_PROJECT any clear -
STORYHOOK_ACTOR any clear -
STORYHOOK_ALLOW_TEMP_PROJECT any clear -
STORYHOOK_ALLOW_PROJECT_BURST any clear -
STORYHOOK_ALLOW_UNINSTALLED_MIGRATION any clear -
STORYHOOK_VERIFIER_MIRROR any literal 0
TABLE
}

# Parses the shared options. Sets `_sti_root`, `_sti_home`, `_sti_pid`.
_storyhook_isolate_args() {
    _sti_home=0
    _sti_pid="$$"
    _sti_root=""

    while [ "$#" -gt 0 ]; do
        case "$1" in
        --home)
            _sti_home=1
            shift
            ;;
        --parent-pid)
            if [ "$#" -lt 2 ]; then
                echo "storyhook_isolate: --parent-pid needs a pid" >&2
                exit 2
            fi
            _sti_pid="$2"
            shift 2
            ;;
        --)
            shift
            break
            ;;
        -*)
            # An argument that lands nowhere is refused, never dropped: a
            # misspelled flag that isolated less than the caller asked for
            # would be invisible in every observable except the damage.
            echo "storyhook_isolate: unknown option [$1]" >&2
            exit 2
            ;;
        *)
            if [ -n "$_sti_root" ]; then
                echo "storyhook_isolate: one root only, got [$_sti_root] and [$1]" >&2
                exit 2
            fi
            _sti_root="$1"
            shift
            ;;
        esac
    done

    if [ -z "$_sti_root" ]; then
        echo "storyhook_isolate: a root directory is required" >&2
        exit 2
    fi

    # Trailing slashes are stripped so the paths built below are spelled the
    # same way `storyhook::env::test_environment::resolve` spells them: the
    # equality test compares strings, and `<root>//home` is a different string
    # for the same directory.
    while [ "${_sti_root%/}" != "$_sti_root" ]; do _sti_root="${_sti_root%/}"; done

    # The one refusal: a root that is not disposable. `/tmp` and `/private/tmp`
    # are the same directory on macOS and both spellings reach it, so both are
    # accepted. `$TMPDIR` is deliberately NOT: it is Spotlight-indexed there,
    # and a fixture-heavy run backlogs `mds_stores` behind it until unrelated
    # tests fail as unexplained 404s (SH-53).
    case "$_sti_root" in
    /tmp/* | /private/tmp/*) ;;
    *)
        echo "storyhook_isolate: refusing to isolate at [$_sti_root]" >&2
        echo "  a test environment's root must be disposable — under /tmp or" >&2
        echo "  /private/tmp — because everything storyhook writes goes inside" >&2
        echo "  it, and the whole point is that deleting it costs nothing." >&2
        exit 1
        ;;
    esac
}

# The value one parameter takes, or the empty string when it is removed.
_storyhook_isolate_value() {
    case "$2" in
    root) [ "$3" = "." ] && printf '%s' "$1" || printf '%s/%s' "$1" "$3" ;;
    literal) printf '%s' "$3" ;;
    ownpid) printf '%s' "$4" ;;
    clear) ;;
    esac
}

storyhook_isolate() {
    _storyhook_isolate_args "$@"

    while read -r _name _scope _kind _arg; do
        [ -n "$_name" ] || continue
        if [ "$_scope" = "story" ] && [ "$_sti_home" -ne 1 ]; then
            continue
        fi
        if [ "$_kind" = "clear" ]; then
            unset "$_name"
            continue
        fi
        _value="$(_storyhook_isolate_value "$_sti_root" "$_kind" "$_arg" "$_sti_pid")"
        export "$_name=$_value"
    done <<EOF
$(_storyhook_test_environment)
EOF

    # The directories a `story` process expects to find. Derived from the table
    # rather than listed, so a parameter added above cannot leave its directory
    # uncreated. The store file itself is not created — the daemon makes it, and
    # a zero-byte file where a database belongs is worse than no file.
    mkdir -p \
        "$_sti_root/home" \
        "$_sti_root/home/.local/share" \
        "$_sti_root/home/.config" \
        "$_sti_root/home/.local/state" \
        "$_sti_root/home/.local/share/storyhook"

    unset _name _scope _kind _arg _value
}

# What `storyhook_isolate` would do, printed as shell for a caller that wants
# to hand the environment to something else rather than adopt it here.
#
# Reads the same table, so it cannot claim one thing while the function does
# another. Values are single-quoted (with embedded quotes escaped) because a
# fixture root is a path and paths contain spaces on machines whose owners have
# spaces in their names.
storyhook_isolate_print() {
    _storyhook_isolate_args "$@"

    while read -r _name _scope _kind _arg; do
        [ -n "$_name" ] || continue
        if [ "$_scope" = "story" ] && [ "$_sti_home" -ne 1 ]; then
            continue
        fi
        if [ "$_kind" = "clear" ]; then
            printf 'unset %s\n' "$_name"
            continue
        fi
        _value="$(_storyhook_isolate_value "$_sti_root" "$_kind" "$_arg" "$_sti_pid")"
        printf "export %s='%s'\n" "$_name" "$(printf '%s' "$_value" | sed "s/'/'\\\\''/g")"
    done <<EOF
$(_storyhook_test_environment)
EOF

    unset _name _scope _kind _arg _value
}
