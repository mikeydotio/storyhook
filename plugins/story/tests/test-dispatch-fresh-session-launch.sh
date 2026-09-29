#!/usr/bin/env bash
# SH-850 council C1: every launch -- fresh or resumed, Claude or Codex,
# attended, autonomous or Full Auto -- starts a NEW provider session. The
# council rejected reviving a lost conversation (claude --resume/--continue/
# --fork-session, codex resume): the Codex bootstrap accepts only a startup
# SessionStart, a revived id breaks the rule that a replacement session is new,
# the witness naming the old id is untrusted, and a revived transcript carries
# the charter of the plugin version that first dispatched it. SH-854 holds the
# redesign trigger. This pins the launch argv the helper renders, so a native
# resume cannot creep into any template.
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo RFS)
fresh=$(new_story "$repo" "Never dispatched")
resumed=$(new_story "$repo" "Claimed before its agent was lost")
(cd "$repo" && story move "$resumed" in-progress >/dev/null)

render() {
  local agent="$1"
  shift
  (cd "$repo" && STORY_DRY_RUN=1 STORY_COUNCIL=off STORY_AGENT="$agent" \
    bash "$SCRIPT" dispatch "$@" 2>&1)
}

for agent in claude codex; do
  for autonomy in attended auto full-auto; do
    case "$autonomy" in
      attended) flags=() ;;
      auto) flags=(--auto) ;;
      full-auto) flags=(--auto --full-auto) ;;
    esac
    for target in fresh resumed; do
      if [ "$target" = fresh ]; then
        out=$(render "$agent" "$fresh" --agent="$agent" ${flags[@]+"${flags[@]}"})
      else
        out=$(render "$agent" "$resumed" --agent="$agent" --resume ${flags[@]+"${flags[@]}"})
      fi
      label="$agent $autonomy $target"
      assert_eq "$(jqf "$out" .ok)" "true" "$label: the dry run renders"
      [ "$target" = fresh ] || assert_eq "$(jqf "$out" .resumed)" "true" "$label: it is a resume"
      commands=$(jqf "$out" '.commands | join(" ")')
      launch=${commands#* -P -F #\{pane_id\} }
      launch=${launch%% \\; set-window-option*}
      [ "$launch" != "$commands" ] || fail_test "$label: the launch command was located in [$commands]"
      # Claude spells revival --resume/-r, --continue/-c and --fork-session;
      # Codex spells it as the `resume` or `fork` subcommand (its own `-c` is
      # a config override, not a revival).
      case "$agent:$launch" in
        *" --resume"* | *" --continue"* | *" --fork-session"* \
          | claude:*" -c "* | claude:*" -r "* \
          | codex:*" resume"* | codex:*" fork"*)
          fail_test "$label: the launch revives a native conversation: $launch" ;;
      esac
    done
  done
done

finish
