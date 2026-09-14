"""Run a tmux lifecycle command without exporting caller workspace authority."""

import os
import subprocess
import sys


def main():
    """Retain the caller's lock here while the detached terminal boundary runs."""
    environment = os.environ.copy()
    environment.pop("STORY_WORKSPACE_LOCK_FD", None)
    # The guardian keeps inherited locks until the client exits. A new server
    # and its panes must not inherit them and outlive the dispatch handoff.
    result = subprocess.run(["tmux", *sys.argv[1:]], env=environment, close_fds=True)
    return result.returncode if result.returncode >= 0 else 128 - result.returncode


if __name__ == "__main__":
    sys.exit(main())
