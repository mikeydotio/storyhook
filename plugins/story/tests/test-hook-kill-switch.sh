#!/usr/bin/env bash
# The session hooks' `enabled = false` kill switch, and where they look for it.
#
# Two properties, and the suite has never covered either:
#
#   1. **The switch works.** SH-47: the old reader stripped quotes with two
#      `sed` expressions that each required a literal `"`, so an *unquoted*
#      boolean matched neither and the whole line `enabled = false` came back —
#      which is not the string `false`, so the documented switch did nothing.
#      The unquoted form is exactly what storyhook's own template writes, so
#      the only way to disable the hooks was to write a value storyhook never
#      generates.
#   2. **It is read from the right place, and only from the right place.**
#      Configuration lives in `.storyhook.toml`'s `[plugin]` table now, beside
#      the project's identity, so a reader that matches a bare `^key` anywhere
#      in the file would read `prefix` or `schema` as plugin configuration.
#
# Observed through a fake `story` on PATH: each hook is asked to do its job and
# the fake records that it was reached. "Disabled" is the absence of that
# record, which is the only thing a no-op can be observed by.
source "$(dirname "$0")/lib.sh"

HOOKS_DIR="$PLUGIN_ROOT/hooks"

FAKE_DIR=$(mktemp -d /tmp/story-test-hookfake.XXXXXX)
_TMP_REPOS+=("$FAKE_DIR")
export STORY_FAKE_LOG="$FAKE_DIR/reached"
cat >"$FAKE_DIR/story" <<'FAKE'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$STORY_FAKE_LOG"
printf 'the fake story CLI ran\n'
FAKE
chmod +x "$FAKE_DIR/story"
export PATH="$FAKE_DIR:$PATH"

repo=$(mktemp -d /tmp/story-test-hookrepo.XXXXXX)
_TMP_REPOS+=("$repo")
mkdir -p "$repo/deep/nested"

# The stdin payload post-git.sh reacts to. Everything else it ignores.
GIT_EVENT='{"tool_input":{"command":"git commit -m wip"}}'

# write_pointer <body> — replace the fixture's pointer file.
write_pointer() { printf '%s' "$1" >"$repo/.storyhook.toml"; }

# run_hook <hook> <dir> — run one hook in <dir> with a fresh reach log, echo
# its stdout.
run_hook() {
  local hook="$1" dir="$2" out
  : >"$STORY_FAKE_LOG"
  if [ "$hook" = post-git ]; then
    out=$(cd "$dir" && printf '%s' "$GIT_EVENT" | bash "$HOOKS_DIR/post-git.sh" 2>/dev/null)
  else
    out=$(cd "$dir" && bash "$HOOKS_DIR/stop-handoff.sh" </dev/null 2>/dev/null)
  fi
  printf '%s' "$out"
}

# reached — did the hook get as far as running `story`?
reached() { [ -s "$STORY_FAKE_LOG" ] && printf 'yes' || printf 'no'; }

IDENTITY='schema = 1
uuid = "5f2a1c30-0000-4000-8000-000000000001"
prefix = "SH"
'

for hook in post-git stop-handoff; do
  # --- the baseline: no `[plugin]` table at all means enabled ---
  write_pointer "$IDENTITY"
  out=$(run_hook "$hook" "$repo")
  assert_eq "$(reached)" "yes" "$hook: a pointer with no [plugin] table defaults to enabled"

  # --- SH-47: the unquoted form, which is what storyhook itself writes ---
  # The second key is deliberate: `[plugin]` is a user-authored table, so the
  # reader must pick `enabled` out of one that carries keys it has never heard
  # of rather than reading whatever line comes first.
  write_pointer "$IDENTITY
[plugin]
enabled = false
some-key-this-plugin-never-defined = \"ignored\"
"
  out=$(run_hook "$hook" "$repo")
  assert_eq "$(reached)" "no" "$hook: unquoted enabled = false disables the hook"
  assert_eq "$out" "{}" "$hook: a disabled hook emits an empty directive"

  # --- the quoted form keeps working ---
  write_pointer "$IDENTITY
[plugin]
enabled = \"false\"
"
  out=$(run_hook "$hook" "$repo")
  assert_eq "$(reached)" "no" "$hook: quoted enabled = \"false\" disables the hook"

  # --- enabled = true is not a disable ---
  write_pointer "$IDENTITY
[plugin]
enabled = true
"
  out=$(run_hook "$hook" "$repo")
  assert_eq "$(reached)" "yes" "$hook: enabled = true runs the hook"

  # --- table awareness: a top-level `enabled` is not plugin configuration ---
  write_pointer "enabled = false
$IDENTITY
[plugin]
enabled = true
"
  out=$(run_hook "$hook" "$repo")
  assert_eq "$(reached)" "yes" \
    "$hook: a bare enabled outside [plugin] is not read as plugin configuration"

  # --- a later table does not leak into [plugin] ---
  write_pointer "$IDENTITY
[plugin]
enabled = true

[hooks]
enabled = false
"
  out=$(run_hook "$hook" "$repo")
  assert_eq "$(reached)" "yes" "$hook: a key in a later table is not read as [plugin]'s"

  # --- the ancestor walk: a hook fired from a subdirectory sees the project ---
  write_pointer "$IDENTITY
[plugin]
enabled = false
"
  out=$(run_hook "$hook" "$repo/deep/nested")
  assert_eq "$(reached)" "no" "$hook: the pointer is found from a subdirectory"

  write_pointer "$IDENTITY"
  out=$(run_hook "$hook" "$repo/deep/nested")
  assert_eq "$(reached)" "yes" "$hook: an enabled project works from a subdirectory too"

  # --- no pointer anywhere: the shell stops deciding (SH-119) ---
  #
  # This used to assert the opposite, and the premise is rewritten rather than
  # relaxed. A checkout with no committed pointer file is not "not a storyhook
  # project": it is a fresh clone, which resolves by its registered origin —
  # something only storyhook can look up. So the hook asks, and the command it
  # was going to run anyway is the gate.
  rm -f "$repo/.storyhook.toml"
  out=$(run_hook "$hook" "$repo")
  assert_eq "$(reached)" "yes" \
    "$hook: with no pointer file the hook asks storyhook rather than guessing"
done

# --- and a refusal is silent: the CLI's own answer collapses to `{}` --------
#
# The other half of moving the gate. `story` writes a refusal to *stderr* and
# nothing to stdout, so a directory that is genuinely no project produces an
# empty capture and the hook emits an empty directive — the same thing the
# deleted shell walk produced, without the shell having to know anything.
REFUSING_DIR=$(mktemp -d /tmp/story-test-hookrefuse.XXXXXX)
_TMP_REPOS+=("$REFUSING_DIR")
cat >"$REFUSING_DIR/story" <<'REFUSE'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$STORY_FAKE_LOG"
printf 'error: storyhook cannot tell which project this is.\n' >&2
exit 3
REFUSE
chmod +x "$REFUSING_DIR/story"

for hook in post-git stop-handoff; do
  out=$(PATH="$REFUSING_DIR:$PATH" run_hook "$hook" "$repo")
  assert_eq "$(reached)" "yes" "$hook: the refusing CLI was reached"
  assert_eq "$out" "{}" "$hook: a refusal emits an empty directive"
done

# --- post-git still ignores everything that is not a git write ------------
write_pointer "$IDENTITY"
: >"$STORY_FAKE_LOG"
out=$(cd "$repo" && printf '%s' '{"tool_input":{"command":"ls -la"}}' |
  bash "$HOOKS_DIR/post-git.sh" 2>/dev/null)
assert_eq "$(reached)" "no" "post-git: a non-git command is ignored"
assert_eq "$out" "{}" "post-git: and says nothing"

finish
