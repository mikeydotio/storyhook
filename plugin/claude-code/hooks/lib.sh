#!/usr/bin/env bash
# Shared helpers for storyhook's Claude Code session hooks.
#
# Both hooks have to answer the same two questions before they do anything —
# "am I standing in a storyhook project?" and "has the user turned me off?" —
# and both used to answer them with their own copy of the same nine lines. One
# copy is enough, and one copy is what the tests can hold to account.

# storyhook_pointer — the nearest ancestor's `.storyhook.toml`, if there is one.
#
# **Where the repository's configuration lives, and nothing more.** It used to
# be the whole of "am I in a storyhook project?", and SH-119 took that job away
# from it: a project is identified by its committed pointer file, its registered
# git origin, or an explicit selector, and only storyhook knows which. A shell
# walk cannot see an origin, so gating on this file would silently skip a fresh
# clone that has no pointer yet — exactly the arrangement the new design exists
# to support. Each hook asks storyhook instead, by running the command it was
# going to run anyway; a directory that is no project gets a refusal, on stderr,
# and the hook emits `{}`.
#
# What still reads this file is the `[plugin]` kill switch below, because that
# genuinely is configuration about *this repository* rather than a fact about
# which project it is.
#
# Walking upward is not a nicety. An agent works from subdirectories, and
# configuration set at the repository root has to reach them.
#
# Returns 1 with no output when there is no pointer above the working
# directory, which `read_plugin_config` reads as "use the default".
storyhook_pointer() {
  local dir
  dir=$(pwd -P) || return 1
  while :; do
    if [ -f "$dir/.storyhook.toml" ]; then
      printf '%s' "$dir/.storyhook.toml"
      return 0
    fi
    [ "$dir" = "/" ] && return 1
    dir=$(dirname "$dir")
  done
}

# read_plugin_config <key> <default> — one value from the pointer file's
# `[plugin]` table, or <default>.
#
# Two properties, both load-bearing, both previously absent (SH-47).
#
# **Quote-agnostic.** The old reader stripped quotes with two `sed` expressions
# that each required a literal `"`. Given an *unquoted* TOML boolean neither
# substitution matched, so `val` came back as the whole line — `enabled = false`
# — which is not the string `false`, and the hook proceeded as though enabled.
# Only the non-idiomatic `enabled = "false"` ever worked, and the unquoted form
# is what storyhook's own scaffolding writes. The documented kill switch was
# therefore inoperative for every user who followed the documented instructions.
#
# **Table-aware.** `[plugin]` used to be the only table in a file of its own.
# It now shares `.storyhook.toml` with the project's identity, so a blind
# `^key` match would happily read `prefix`, `schema` or a key from some future
# `[hooks]` table as plugin configuration.
#
# Fails open at every step: an unreadable or malformed pointer yields the
# default rather than an error, because a configuration file nobody can parse
# must not be able to disable — or silently enable — a hook.
read_plugin_config() {
  local key="$1" default="$2" pointer val
  pointer=$(storyhook_pointer) || {
    printf '%s' "$default"
    return
  }
  val=$(awk -v key="$key" -v sq="'" '
    {
      line = $0
      sub(/^[ \t]+/, "", line)
      sub(/[ \t\r]+$/, "", line)
    }
    line ~ /^\[/ {
      table = line
      sub(/^\[[ \t]*/, "", table)
      sub(/[ \t]*\]$/, "", table)
      next
    }
    table != "plugin" { next }
    {
      eq = index(line, "=")
      if (eq == 0) next
      k = substr(line, 1, eq - 1)
      sub(/[ \t]+$/, "", k)
      if (k != key) next

      v = substr(line, eq + 1)
      sub(/^[ \t]+/, "", v)
      q = substr(v, 1, 1)
      if (q == "\"" || q == sq) {
        v = substr(v, 2)
        close_at = index(v, q)
        if (close_at > 0) v = substr(v, 1, close_at - 1)
      } else {
        # An inline comment is only a comment outside a string, which is why
        # this arm is the unquoted one.
        sub(/[ \t]*#.*$/, "", v)
        sub(/[ \t]+$/, "", v)
      }
      print v
      exit
    }
  ' "$pointer" 2>/dev/null || true)
  printf '%s' "${val:-$default}"
}

# hook_is_enabled — whether this project has turned the session hooks off.
#
# The switch is `enabled = false` in the pointer file's `[plugin]` table.
# Anything else, including an absent table and an unreadable file, means on.
hook_is_enabled() {
  [ "$(read_plugin_config enabled true)" != "false" ]
}
