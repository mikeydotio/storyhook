#!/bin/sh
set -eu

BINARY="story"
INSTALL_DIR="${STORYHOOK_INSTALL_DIR:-${HOME}/.local/bin}"
BINARY_PATH="${INSTALL_DIR}/${BINARY}"

if [ ! -f "$BINARY_PATH" ]; then
  echo "error: storyhook binary not found at ${BINARY_PATH}" >&2
  echo "If installed elsewhere, set STORYHOOK_INSTALL_DIR." >&2
  exit 1
fi

echo "Uninstalling storyhook..."

# Remove git hooks (if in a git repo)
if git rev-parse --git-dir >/dev/null 2>&1; then
  echo "  Removing git hooks..."
  "$BINARY_PATH" hooks uninstall 2>/dev/null | sed 's/^/  /' || true
fi

# Remove binary
rm -f "$BINARY_PATH"
echo ""
echo "storyhook has been uninstalled."
echo ""
echo "Note: your stories were NOT removed. They live in one store:"
echo "  ${XDG_DATA_HOME:-$HOME/.local/share}/storyhook/store.db"
echo "Delete that file to remove every project's data, and"
echo "  ${XDG_STATE_HOME:-$HOME/.local/state}/storyhook/"
echo "for the daemon's runtime files and its backups."
