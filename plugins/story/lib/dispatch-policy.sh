#!/usr/bin/env bash
# Fill missing selectors from the CLI's validated policy. cmd_dispatch owns
# these variables so --next can retain one pre-claim policy snapshot.
apply_complexity_policy() {
  model_source=flag effort_source=flag policy_note=""
  [ -n "$requested_model" ] || model_source=environment
  [ -n "$requested_effort" ] || effort_source=environment

  # A guarded continuation is a recorded launch, not a fresh policy decision.
  if [ -n "$require_absent" ]; then
    if [ -z "$requested_model" ]; then
      resolved_model=$(printf '%s' "$continuation_record" | jq -r '.capture.model // ""')
      model_source=continuation
    fi
    if [ -z "$requested_effort" ]; then
      resolved_effort=$(printf '%s' "$continuation_record" | jq -r '.capture.effort // ""')
      effort_source=continuation
    fi
    return
  fi

  if { [ -n "$full_auto" ] && [ "$FULL_AUTO_LAUNCH_OVERRIDDEN" = true ]; } \
      || { [ -z "$full_auto" ] && [ "$AUTO_LAUNCH_OVERRIDDEN" = true ]; }; then
    model_source=custom-command effort_source=custom-command
    policy_note="Custom launch command controls model and effort; complexity mapping was skipped."
    return
  fi
  # Epics launch no provider session. Their engine resolves each child.
  if [ -n "$show_json" ] && [ "$(printf '%s' "$show_json" | jq -r '.story.story.story_type // ""')" = epic ]; then
    model_source=child-story effort_source=child-story
    return
  fi
  [ -z "$resolved_model" ] || [ -z "$resolved_effort" ] || return 0

  if [ -z "$policy_json" ]; then
    policy_json=$(story_cli dispatch-policy show --json) \
      || fail "cannot read dispatch policy: $policy_json"
    printf '%s' "$policy_json" | jq -e '.result == "ok" and (.dispatch_policy.entries | length == 6)' >/dev/null \
      || fail "dispatch policy returned an invalid document: $policy_json"
  fi
  # NEXT MODE has validated the complete policy before its atomic claim.
  # It calls this function again with the actual claim's snapshot.
  [ -n "$show_json" ] || return 0
  selected_complexity=$(printf '%s' "$show_json" | jq -r '.story.story.complexity // "medium"')
  local selection
  selection=$(printf '%s' "$policy_json" | jq -ce --arg agent "$AGENT" --arg level "$selected_complexity" \
    '[.dispatch_policy.entries[] | select(.agent == $agent and .complexity == $level)] | if length == 1 then .[0] else error("missing policy row") end') \
    || fail "no dispatch policy for $AGENT complexity $selected_complexity"
  if [ -z "$resolved_model" ]; then
    resolved_model=$(printf '%s' "$selection" | jq -r .model)
    model_source=$(printf '%s' "$selection" | jq -r .model_source)
  fi
  if [ -z "$resolved_effort" ]; then
    resolved_effort=$(printf '%s' "$selection" | jq -r .effort)
    effort_source=$(printf '%s' "$selection" | jq -r .effort_source)
  fi
  validate_agent_model "$resolved_model"
  validate_agent_effort "$resolved_effort"
}
