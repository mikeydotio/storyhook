#!/bin/sh
set -eu

REPO="mikeydotio/storyhook"
BINARY="story"
INSTALL_DIR="${STORYHOOK_INSTALL_DIR:-${HOME}/.local/bin}"

# Detect OS
OS="$(uname -s)"
case "$OS" in
  Linux)  os="unknown-linux-gnu" ;;
  Darwin) os="apple-darwin" ;;
  *)      echo "error: unsupported OS: $OS" >&2; exit 1 ;;
esac

# Detect architecture
ARCH="$(uname -m)"
case "$ARCH" in
  x86_64|amd64)  arch="x86_64" ;;
  aarch64|arm64)  arch="aarch64" ;;
  *)              echo "error: unsupported architecture: $ARCH" >&2; exit 1 ;;
esac

TARGET="${arch}-${os}"
ARTIFACT="story-${TARGET}.tar.gz"

# Resolve latest version if not pinned
VERSION="${STORYHOOK_VERSION:-latest}"
if [ "$VERSION" = "latest" ]; then
  VERSION="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
    | grep '"tag_name"' | head -1 | cut -d'"' -f4)"
fi

if [ -z "$VERSION" ]; then
  echo "error: could not determine latest release version" >&2
  exit 1
fi

URL="https://github.com/${REPO}/releases/download/${VERSION}/${ARTIFACT}"

echo "Installing ${BINARY} ${VERSION} (${TARGET})..."
echo "  from: ${URL}"
echo "  to:   ${INSTALL_DIR}/${BINARY}"

# Create install directory
mkdir -p "$INSTALL_DIR"

# Download and extract
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

curl -fsSL "$URL" -o "${TMPDIR}/${ARTIFACT}"
tar xzf "${TMPDIR}/${ARTIFACT}" -C "$TMPDIR"
install -m 755 "${TMPDIR}/${BINARY}" "${INSTALL_DIR}/${BINARY}"

echo ""
echo "Installed ${BINARY} to ${INSTALL_DIR}/${BINARY}"

echo ""
echo "To use storyhook as an MCP server for AI tools (Claude Code, Cursor):"
echo "  story mcp-config"

# Check if install dir is in PATH
case ":${PATH}:" in
  *":${INSTALL_DIR}:"*) ;;
  *)
    echo ""
    echo "WARNING: ${INSTALL_DIR} is not in your PATH."
    echo "Add it by appending this to your shell profile:"
    echo ""
    echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
    ;;
esac
