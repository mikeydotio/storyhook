#!/usr/bin/env bash
# Source inside one private Make recipe. Only gate_finish returns ordinary
# failures; cancellation and damaged control state stop the recipe immediately.

gate_cleanup() {
    [ -z "${gate_outcome:-}" ] || rm -f "$gate_outcome"
}

gate_init() {
    gate_status=0
    gate_shared_failure=""
    gate_build_failure=""
    gate_outcome="$(mktemp /tmp/storyhook-gate-outcome.XXXXXX)" || exit 125
    trap gate_cleanup EXIT
    trap 'exit 129' HUP
    trap 'exit 130' INT
    trap 'exit 143' TERM
}

gate_skip() {
    local label="$1" dependency="$2"
    echo "leg $label: SKIPPED — dependency $dependency failed" >&2
    bash scripts/gate-progress.sh item "release gate/$label" skipped \
        "dependency=\"$dependency\"" || exit 125
}

gate_run() {
    local label="$1" status=0 confirmation=0
    shift
    case "$label" in
    (rust-contracts | build)
        if [ -n "$gate_shared_failure" ]; then
            gate_skip "$label" "$gate_shared_failure"
            [ "$label" != build ] || gate_build_failure=build
            return 0
        fi
        ;;
    (plugin | e2e)
        if [ -n "$gate_build_failure" ]; then
            gate_skip "$label" "$gate_build_failure"
            return 0
        fi
        ;;
    esac
    : >"$gate_outcome" || exit 125
    case "$label" in
    (rust-suite | rust-contracts)
        STORYHOOK_GATE_BUILD_OUTCOME="$gate_outcome" "$@" || status=$?
        ;;
    (*) "$@" || status=$? ;;
    esac
    # These statuses cannot be turned into permission to start another leg.
    if [ "$status" -ge 125 ]; then
        echo "gate: $label stopped with control/cancellation status $status" >&2
        exit "$status"
    fi
    case "$label" in
    (rust-suite | rust-contracts)
        python3 scripts/cargo_diagnostics.py --validate-outcome "$gate_outcome" || exit 125
        ;;
    esac
    if [ "$status" -ne 0 ]; then
        bash scripts/gate-progress.sh item "release gate/$label" failed || exit 125
        echo "leg $label: FAILED — exit $status" >&2
        [ "$gate_status" -ne 0 ] || gate_status="$status"
        case "$label" in
        (build) gate_build_failure=build ;;
        (rust-suite | rust-contracts)
            python3 scripts/cargo_diagnostics.py --confirm-shared "$gate_outcome" || confirmation=$?
            case "$confirmation" in
            (0) ;;
            (10) gate_shared_failure="$label" ;;
            (*) exit "$confirmation" ;;
            esac
            ;;
        esac
    fi
}

gate_finish() {
    exit "$gate_status"
}
