---
name: storyhook-install
description: "Use when the storyhook 'story' CLI is missing (command not found) and an operation needs it, or when explicitly asked to install the storyhook CLI. Installs the story binary via the official installer or cargo, then verifies. Useful after installing the plugin via the Claude Code marketplace, which does not include the CLI."
user-invocable: true
allowed-tools: Bash(command -v *), Bash(which *), Bash(story *), Bash(cargo *), Bash(curl *), Bash(uname *), AskUserQuestion
---

# Storyhook Install

Install the storyhook `story` CLI binary. The Claude Code plugin (skills and hooks)
needs this CLI to do anything useful; installing via the marketplace does not bring
the CLI with it, so this skill bootstraps it.

## Steps

### 1. Check whether the CLI is already installed

Run `command -v story`.

- **If found**, the CLI is already installed. Confirm it works by running `story doctor`
  (or `story --help` — note there is no `--version` flag) and stop here. Nothing to do.
- **If not found**, continue to step 2.

### 2. Ask permission and choose an install method

Use `AskUserQuestion` to get explicit permission to install and to pick a method:

- **Official installer (recommended)** — downloads a prebuilt binary to `~/.local/bin/story`:
  ```bash
  curl -fsSL https://raw.githubusercontent.com/mikeydotio/storyhook/main/install.sh | bash
  ```
- **Cargo (build from source)** — requires a Rust toolchain:
  ```bash
  cargo install storyhook
  ```

If the user declines, stop and explain that storyhook operations can't run without the CLI.

### 3. Run the chosen install command

Run the command the user selected.

- The official installer may also offer to install git hooks if it's run inside a git repo;
  let it prompt as normal.

### 4. Verify the install

Run `story --help` to confirm the binary is on `PATH` and working.

- **If `story` is still not found**, the install directory is probably not on `PATH`.
  Tell the user to add `~/.local/bin` to their `PATH` (the official installer prints the
  exact line), e.g.:
  ```bash
  export PATH="$HOME/.local/bin:$PATH"
  ```
  They will need to restart their shell (and Claude Code session) for it to take effect.

### 5. Next steps

Once the CLI is verified, suggest:

- `/storyhook:storyhook-setup` to initialize the project and configure plugin behavior, or
- `/storyhook:storyhook-context` to see the current project state and start working.
