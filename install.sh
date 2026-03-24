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

# --- Interactive setup starts here ---

# Detect if we can prompt interactively
if [ -t 0 ]; then
  TTY_IN="/dev/stdin"
elif [ -e /dev/tty ]; then
  TTY_IN="/dev/tty"
else
  TTY_IN=""
fi

if [ -n "$TTY_IN" ]; then
  echo ""
  echo "=== MCP Server Configuration ==="
  echo "storyhook can register as an MCP server for AI coding tools."
  echo ""

  printf "  Configure for Claude Code? [Y/n] " > /dev/tty
  read -r ans < "$TTY_IN" || ans=""
  case "$ans" in
    [Nn]*) ;;
    *)
      if "${INSTALL_DIR}/${BINARY}" mcp-config --install claude 2>&1; then
        echo "    done."
      else
        echo "    skipped (could not configure)."
      fi
    ;;
  esac

  printf "  Configure for Codex CLI? [Y/n] " > /dev/tty
  read -r ans < "$TTY_IN" || ans=""
  case "$ans" in
    [Nn]*) ;;
    *)
      if "${INSTALL_DIR}/${BINARY}" mcp-config --install codex 2>&1; then
        echo "    done."
      else
        echo "    skipped (could not configure)."
      fi
    ;;
  esac

  # Git hooks (only if in a git repo)
  if git rev-parse --git-dir >/dev/null 2>&1; then
    echo ""
    echo "=== Git Hooks ==="
    echo "Available hooks:"
    echo "  - post-commit: link commits to referenced stories"
    echo "  - post-merge: auto-close stories on merge to main"
    echo "  - prepare-commit-msg: show top story in commit template"
    echo ""
    printf "  Install git hooks in this repository? [Y/n] " > /dev/tty
    read -r ans < "$TTY_IN" || ans=""
    case "$ans" in
      [Nn]*) ;;
      *)
        if "${INSTALL_DIR}/${BINARY}" hooks install 2>&1; then
          echo "    done."
        else
          echo "    skipped (could not install hooks)."
        fi
      ;;
    esac
  fi
else
  # Non-interactive fallback
  echo ""
  echo "To configure MCP integration: story mcp-config"
  echo "To install git hooks:         story hooks install"
fi

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
