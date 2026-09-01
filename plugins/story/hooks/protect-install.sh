#!/usr/bin/env bash
# Refuse edits to storyhook's own INSTALLED copies (SH-530).
#
# An agent that edits `plugins/story/bin/story.sh` in a checkout is doing the
# right thing: the change is reviewed, gated and shipped by the next release.
# An agent that edits the INSTALLED copy of the same file is doing something
# that is wrong twice over, and quietly:
#
#   1. it drifts the installation away from the release it claims to be, which
#      is the whole subject of SH-530; and
#   2. the next `story plugin install` overwrites it, so the change is LOST as
#      well as unversioned and untested.
#
# (2) is why this hook redirects instead of merely refusing. The work is
# usually good work aimed at the wrong file, and a refusal that does not say
# where the file actually lives just invites a second attempt.
#
# WHAT THIS IS NOT. It is not a security boundary and it is not the last line
# of defence. A Claude Code `PreToolUse` hook FAILS OPEN at its timeout -- the
# harness SIGTERMs it and lets the tool call proceed, silently (SH-306) -- so
# this is the cheap layer that catches the common case at the moment of the
# act. Reading the installation afterwards is the authoritative check, because
# it sees the result regardless of which agent, or which provider, did it.
#
# SPEED IS THEREFORE CORRECTNESS. The inert path pays no interpreter start, the
# discipline `full-auto.sh` already states: this reads stdin once and, if the
# raw payload does not mention a managed prefix as a plain substring, prints
# `{}` and exits in shell. Only a substring hit pays for python3. The substring
# test is a superset of true hits, so it errs safe. Nothing here runs `story`
# (the binary may be mid-replacement) or talks to the daemon.
#
# COVERAGE IS ASYMMETRIC AND SAID SO RATHER THAN GLOSSED. Claude Code runs this
# on Write/Edit/NotebookEdit and Bash. Codex registers only `post_tool_use`,
# `session_start` and `stop` from this same hooks.json -- it appears not to take
# `pre_tool_use` at all -- so on Codex this hook does not run, and that gap is
# named here rather than glossed.

set -uo pipefail

emit_inert() { printf '{}'; exit 0; }

payload=""
if [ ! -t 0 ]; then
  payload="$(cat)"
fi
[ -n "$payload" ] || emit_inert

# Resolved the way `crate::env` resolves the data home, and the way
# `plugin::managed_paths_file` writes it. No manifest means no plugin install
# has happened here, so there is nothing installed to protect.
if [ -n "${STORYHOOK_DATA_DIR:-}" ]; then
  manifest="${STORYHOOK_DATA_DIR}/managed-paths"
elif [ -n "${XDG_DATA_HOME:-}" ]; then
  manifest="${XDG_DATA_HOME}/storyhook/managed-paths"
else
  manifest="${HOME}/.local/share/storyhook/managed-paths"
fi
[ -f "$manifest" ] || emit_inert

# The deliberate, discoverable escape hatch. A file rather than an environment
# variable (SH-411): a variable can sit exported in a shell for a week and
# authorize every later edit invisibly, where creating a file is a deliberate
# act that leaves a dated trace on disk.
override="$(dirname "$manifest")/ALLOW_INSTALLED_EDITS"
[ -e "$override" ] && emit_inert

# The fast prefilter: one substring test per managed prefix, no subprocess.
hit=""
while IFS= read -r prefix; do
  case "$prefix" in ''|'#'*) continue ;; esac
  case "$payload" in *"$prefix"*) hit="$prefix"; break ;; esac
done < "$manifest"
[ -n "$hit" ] || emit_inert

# Only now is it worth parsing. The prefilter said a managed path appears
# SOMEWHERE in the payload; this decides whether it is actually the target.
STORYHOOK_MANIFEST="$manifest" python3 -c '
import json, os, sys

payload = json.load(sys.stdin)
tool = payload.get("tool_name", "")
supplied = payload.get("tool_input", {}) or {}

manifest = os.environ["STORYHOOK_MANIFEST"]
override_dir = os.path.dirname(manifest)

prefixes = []
with open(manifest, encoding="utf-8") as handle:
    for line in handle:
        line = line.strip()
        if line and not line.startswith("#"):
            prefixes.append(line)

# Where a path can hide, per tool. `Bash` carries a whole command line, so the
# whole string is searched: `sed -i`, `tee`, `cp`, `install` and `rm` all reach
# these files, and a hook that guarded only the structured editors would be
# bypassed by the shell it left wide open.
if tool == "Bash":
    haystacks = [str(supplied.get("command", ""))]
else:
    haystacks = [
        str(supplied.get(key, ""))
        for key in ("file_path", "notebook_path", "path")
    ]

target = next(
    (
        prefix
        for prefix in prefixes
        for haystack in haystacks
        if prefix and prefix in haystack
    ),
    None,
)

if target is None:
    sys.stdout.write("{}")
    raise SystemExit(0)

reason = (
    f"storyhook: refusing to edit an installed release artifact.\n"
    f"  {target} is written by `story plugin install`, so an edit there is\n"
    f"  overwritten by the next install -- lost, unversioned and untested --\n"
    f"  and it drifts this machine away from the release it reports.\n"
    f"\n"
    f"  Make the change in the storyhook CHECKOUT instead (plugins/story/...),\n"
    f"  where it is reviewed, gated and shipped by the next release.\n"
    f"  Nothing you have written is lost by this refusal.\n"
    f"\n"
    f"  If you genuinely mean to edit the installed copy, create\n"
    f"  {override_dir}/ALLOW_INSTALLED_EDITS."
)

sys.stdout.write(
    json.dumps(
        {
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason,
            }
        }
    )
)
' <<<"$payload"
exit 0
