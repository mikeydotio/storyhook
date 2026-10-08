#!/usr/bin/env bash
# SH-891: exercise the real reset helper with a recorded native preview reply.
# Keep the suite's shared isolation and cleanup even though this command's
# native call is replaced below and no reset or resource mutation is performed.
source "$(dirname "$0")/lib.sh"
set -euo pipefail
fixture="$(mktemp -d "${TMPDIR:-/tmp}/storyhook-reset-preview.XXXXXX")"
_TMP_REPOS+=("$fixture")
# Load only the real command function; sourcing story.sh would dispatch argv.
eval "$(sed -n '/^cmd_reset() {/,/^}/p' "${RESET_PREVIEW_HELPER:-$SCRIPT}")"
RESET_USAGE=unused
DRY_RUN=true
REL_FORCE=true
REL_ID=SH-42
REL_COMMENT_MODE=text
REL_COMMENT_TEXT='Must not post this'
_parse_release_args() { :; }
fail() { printf '%s\n' "$*" >&2; exit 1; }
story_cli() {
  printf '%s\n' "$@" >> "$fixture/calls"
  if [ "${PREVIEW_FAIL:-false}" = true ]; then
    printf '{"result":"error","error":"preview unavailable"}\n'
    return 1
  fi
  if [ "${PREVIEW_MALFORMED:-false}" = true ]; then
    printf '{"result":"ok"}\n'
    return 0
  fi
  printf '%s\n' '{"result":"ok","dry_run":true,"preview":{"story_id":"SH-42","worktree":"/fixture/lane","recovery":{"dirty":3,"untracked":2,"tip":"abc","unpushed":1},"residue":[{"resource":"foreign window","reason":"caller owns it"}]},"message":"Observed exact native preview"}'
}
out="$(cmd_reset SH-42 --force --comment "$REL_COMMENT_TEXT")"
printf '%s' "$out" | jq -e '.ok and .dry_run and .preview.recovery.dirty == 3 and .preview.recovery.untracked == 2 and .preview.residue[0].reason == "caller owns it" and .display == "Observed exact native preview"' >/dev/null || fail "native preview facts missing from helper answer"
printf '%s\n' reset SH-42 --force --dry-run --json > "$fixture/expected"
cmp "$fixture/calls" "$fixture/expected"
: > "$fixture/calls"
if (PREVIEW_FAIL=true; cmd_reset SH-42) > "$fixture/failure" 2>&1; then
  printf 'FAIL: native preview error became success\n' >&2
  exit 1
fi
cmp "$fixture/calls" "$fixture/expected"
grep -q 'preview unavailable' "$fixture/failure"
: > "$fixture/calls"
if (PREVIEW_MALFORMED=true; cmd_reset SH-42) > "$fixture/malformed" 2>&1; then
  fail "malformed native preview became success"
fi
cmp "$fixture/calls" "$fixture/expected"
grep -q 'did not return a native plan' "$fixture/malformed"
printf 'PASS: reset preview delegates once, preserves structured facts, posts no comment, and propagates refusal\n'
