#!/usr/bin/env bash
# Package identity is an existing physical directory, never its cache spelling.
canonical_plugin_root() {
  case "${1:-}" in /*) ;; *) return 1 ;; esac
  [ -d "$1" ] || return 1
  (CDPATH= cd -- "$1" && pwd -P) 2>/dev/null
}

plugin_roots_match() {
  local actual expected
  actual=$(canonical_plugin_root "${1:-}") || return 1
  expected=$(canonical_plugin_root "${2:-}") || return 1
  [ "$actual" = "$expected" ]
}

plugin_receipt_matches() {
  local actual
  actual=$(jq -er '.plugin_root | select(type == "string" and length > 0)' "$1" 2>/dev/null) || return 1
  plugin_roots_match "$actual" "$2"
}
