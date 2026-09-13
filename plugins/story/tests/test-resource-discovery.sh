#!/usr/bin/env bash
# SH-709: production reset must find Git resources independently of its caller.
resource_test_tmux=$(command -v tmux)
source "$(dirname "$0")/lib.sh"

# The provider endpoint supplies registration data only. The bridge and helper
# are production code, copied into this fixture's private installed layout.
installed="$HOME/.codex/plugins/cache/storyhook/story/fixture"
mkdir -p "$installed" "$HOME/bin"
ln -s "$resource_test_tmux" "$HOME/bin/tmux"
export TMUX_TMPDIR="$HOME/private-tmux"
mkdir -p "$TMUX_TMPDIR"
cp -R "$PLUGIN_ROOT/." "$installed/"
cat >"$HOME/bin/codex" <<'EOF'
#!/bin/sh
printf '%s\n' '{"installed":[{"pluginId":"story@storyhook","marketplaceName":"storyhook","name":"story","installed":true,"enabled":true,"version":"fixture"}]}'
EOF
chmod +x "$HOME/bin/codex"
export PATH="$HOME/bin:$PATH"
repo=$(mk_story_repo)
slug=$(slug_for "$repo")

for resource in .claude/worktrees .codex/worktrees 'custom lane'; do
  for caller in bridge claude codex terminal unsupported; do
    id=$(new_story "$repo" "Resource $resource caller $caller")
    path="$repo/$resource/$id"
    (cd "$repo" && git worktree add -q -b "worktree-$id" "$path" HEAD) || exit 1
    (cd "$repo" && story claim "$id" --no-comment --json >/dev/null) || exit 1
    out=$(
      cd "$repo" || exit 1
      unset STORY_AGENT TMUX TMUX_PANE
      if [ "$caller" = bridge ]; then
        story plugin run codex -- --project "$slug" reset "$id"
      else
        [ "$caller" = terminal ] || export STORY_AGENT="$caller"
        bash "$SCRIPT" --project "$slug" reset "$id"
      fi
    )
    if [ "$(jqf "$out" .ok)" != true ]; then printf '%s\n' "$out"; fi
    assert_eq "$(jqf "$out" .ok)" true "$resource/$caller: reset succeeds"
    assert_eq "$(jqf "$out" '.removed.worktree')" true "$resource/$caller: removed actual worktree"
    if (cd "$repo" && git worktree list --porcelain | rg -F -- "worktree $path"); then fail_test "$resource/$caller: registration survived"; fi
    [ ! -e "$path" ] || fail_test "$resource/$caller: actual worktree survived"
    (cd "$repo" && git show-ref --verify --quiet "refs/heads/worktree-$id") && fail_test "$resource/$caller: branch survived"
    assert_eq "$(cd "$repo" && story show "$id" --json | jq -r '.story.story.state')" todo "$resource/$caller: claim released"
  done
done

# --force never turns duplicate Git ownership into permission to delete both.
id=$(new_story "$repo" "Ambiguous ownership")
first="$repo/.claude/worktrees/$id"
second="$repo/.codex/worktrees/$id"
(cd "$repo" && git worktree add -q -b "worktree-$id" "$first" HEAD && git worktree add -q --force "$second" "worktree-$id" && story claim "$id" --no-comment >/dev/null)
out=$(cd "$repo" && STORY_AGENT=unsupported bash "$SCRIPT" reset "$id" --force)
assert_eq "$(jqf "$out" .reason)" resource-identity-unsafe "force: ambiguity refuses"
[ -d "$first" ] && [ -d "$second" ] || fail_test "force: a candidate was removed"
assert_eq "$(cd "$repo" && story show "$id" --json | jq -r '.story.story.state')" in-progress "force: claim preserved"

finish
