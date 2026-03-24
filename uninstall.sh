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

# Deregister MCP from all providers
echo "  Removing MCP registrations..."
"$BINARY_PATH" mcp-config --uninstall-all 2>/dev/null | sed 's/^/  /' || true

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
echo "Note: project data in .storyhook/ directories was NOT removed."
echo "To remove project data, delete .storyhook/ in each project."
