#!/usr/bin/env bash
# Shell adapter only: the binary owns origin parsing, credentials and transport.
# The daemon supplies its own binary and the registered checkout as authority.
# Manual script calls use their current checkout; no global project is inferred.
# Pin a multi-step operation in its parent shell; inherited pins are checked,
# never replaced. Each later helper call still resolves the actual origin.
github_begin() {
    local checkout metadata
    local args
    checkout="$(git rev-parse --show-toplevel)" || return 1
    args=(github resolve --checkout "$checkout" --authority "${STORYHOOK_GITHUB_AUTHORITY:-$checkout}")
    if [ -n "${STORYHOOK_GITHUB_EXPECTED:-}" ]; then
        args+=(--expected "$STORYHOOK_GITHUB_EXPECTED")
    fi
    if ! metadata=$("${STORY_BIN:-story}" "${args[@]}" 2>&1); then
        GITHUB_ACCESS_ERROR="$metadata"
        return 1
    fi
    STORYHOOK_GITHUB_EXPECTED=$(printf '%s\n' "$metadata" | jq -er '.identity | [.host, .owner, .repo] | join("/")') || return 1
    export STORYHOOK_GITHUB_EXPECTED
}

# Single-call reads can refresh. Multi-step callers must call github_begin in
# their parent shell, which supplies the pin inherited by every later call.
github_call() {
    local mode="$1" checkout args
    shift
    checkout="$(git rev-parse --show-toplevel)" || return 1
    args=(github "$mode" --checkout "$checkout" --authority "${STORYHOOK_GITHUB_AUTHORITY:-$checkout}")
    if [ -n "${STORYHOOK_GITHUB_EXPECTED:-}" ]; then
        args+=(--expected "$STORYHOOK_GITHUB_EXPECTED")
    fi
    "${STORY_BIN:-story}" "${args[@]}" -- "$@"
}

github_exec() { github_call exec "$@"; }
github_git() { github_call git "$@"; }
