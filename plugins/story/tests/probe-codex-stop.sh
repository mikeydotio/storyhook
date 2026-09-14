#!/usr/bin/env bash
# Opt-in wire probe; requires installed Codex, a local socket, and this build.
source "$(dirname "$0")/lib.sh"
repo=$(mk_story_repo)
id=$(cd "$repo" && story new "Codex Stop probe" --json | jq -r '.story.story.id')
(cd "$repo" && story move "$id" in-progress >/dev/null)
PYTHONDONTWRITEBYTECODE=1 python3 "$TESTS_DIR/probe_codex_stop.py" "$repo" "$id"
