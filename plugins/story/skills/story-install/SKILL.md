---
name: story-install
description: "Use when the storyhook 'story' CLI is missing or when the user explicitly asks to install it. Installs the binary through the official installer or Cargo, with permission, then verifies it."
---

# Storyhook Install

Install the storyhook `story` CLI binary. The Storyhook plugin
needs this CLI to do anything useful; installing a plugin package does not bring
the CLI with it, so this skill bootstraps it.

## Steps

### 1. Check whether the CLI is already installed

Run `command -v story`.

- **If found**, confirm it works by running `story --version`. In Codex, also run
  `story plugin install codex` so the stable Storyhook launcher and its narrow sandbox rule
  are installed or refreshed; then continue to step 5. In other hosts, stop here.
- **If not found**, continue to step 2.

### 2. Ask permission and choose an install method

Ask one concise question to get explicit permission to install and to pick a method. Use
the host's structured question mechanism when available:

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

Run `story --version` to confirm the binary is on `PATH` and working. It prints the
installed version, which is the one fact worth having here — and unlike `story doctor`
it says nothing about any project, so it cannot fail for a reason that has nothing to
do with the install.

- **If `story` is still not found**, the install directory is probably not on `PATH`.
  Tell the user to add `~/.local/bin` to their `PATH` (the official installer prints the
  exact line), e.g.:
  ```bash
  export PATH="$HOME/.local/bin:$PATH"
  ```
  They will need to restart their shell and agent session for it to take effect.

When the active host is Codex, run `story plugin install codex` after the CLI is verified.
That idempotent installer refreshes the plugin, writes the stable
`~/.codex/storyhook/story.sh` launcher, verifies its dedicated Codex rule, and reports both
paths. Do not hand-edit or broadly allowlist `bash`.

### 5. Next steps

Once the CLI is verified, suggest:

- the `story-setup` skill to initialize the project and configure plugin behavior, or
- the `story-context` skill to see the current project state and start working.

For Codex, tell the user to restart Codex before invoking another Storyhook skill so the
new rule and plugin instructions load.
