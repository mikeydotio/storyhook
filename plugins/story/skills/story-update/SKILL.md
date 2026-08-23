---
name: story-update
description: "Use when asked to update, upgrade, or check for a new version of the storyhook 'story' CLI. Checks the installed version against the latest GitHub release and, with permission, self-updates the binary in place via 'story update'."
---

# Storyhook Update

Update the storyhook `story` CLI to the latest published GitHub release. The CLI
self-updates in place with `story update`: it downloads the release asset for the
current platform, verifies it runs, and atomically replaces the running binary.

## Steps

### 1. Confirm the CLI is installed

Run `command -v story`.

- **If not found**, there is nothing to update. Follow the `story-install`
  skill to install the CLI first, then stop.
- **If found**, continue.

### 2. Check whether an update is available

Run `story update --check`. This queries GitHub but changes nothing. It reports
one of:

- an update is available (`story vX -> vY`),
- already up to date, or
- the local build is newer than the latest release (a dev build).

Also run `story --version` to show the currently installed version.

- **If already up to date** (and the user did not explicitly ask to reinstall),
  report the version and stop.
- **If an update is available**, continue to step 3.

### 3. Ask permission, then update

Ask one concise question to confirm the user wants to install the update, using the host's
structured question mechanism when available. On
approval, run `story update`.

- If the binary lives in a directory the user can't write to (e.g.
  `/usr/local/bin`), `story update` will say so and suggest re-running with
  elevated privileges or reinstalling via the official installer. Relay that
  guidance rather than retrying blindly.
- To reinstall or pin-refresh the same version, use `story update --force`.

### 4. Verify

Run `story --version` and confirm it reports the new version.

### 5. Next steps

If the update changed plugin-facing behavior, suggest starting a fresh agent
session so the reloaded CLI and any plugin changes take effect. Then use the
`story-context` skill to resume work.
