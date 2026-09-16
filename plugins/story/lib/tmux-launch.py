"""Run a tmux lifecycle command without exporting caller workspace authority."""

import os
import subprocess
import sys


GITHUB_CREDENTIALS = ("GH_CONFIG_DIR", "GH_TOKEN", "GITHUB_TOKEN",
                      "GH_ENTERPRISE_TOKEN", "GITHUB_ENTERPRISE_TOKEN")


def main():
    """Retain the caller's lock here while the detached terminal boundary runs."""
    environment = os.environ.copy()
    environment.pop("STORY_WORKSPACE_LOCK_FD", None)
    for name in GITHUB_CREDENTIALS:
        environment.pop(name, None)
    arguments = sys.argv[1:]
    # Empty per-pane overrides defeat credentials retained by an existing
    # server, before even the launch shell starts. No shared state is changed.
    for index, argument in enumerate(arguments):
        if argument in ("new-session", "new-window", "respawn-pane"):
            overrides = [part for name in GITHUB_CREDENTIALS for part in ("-e", name + "=")]
            arguments[index + 1:index + 1] = overrides
            break
    # The guardian keeps inherited locks until the client exits. A new server
    # and its panes must not inherit them and outlive the dispatch handoff.
    result = subprocess.run(["tmux", *arguments], env=environment, close_fds=True)
    return result.returncode if result.returncode >= 0 else 128 - result.returncode


if __name__ == "__main__":
    sys.exit(main())
