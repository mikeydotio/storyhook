"""Give provider panes their caller's binary, but no parent GitHub operation.

The client scrub prevents a new server from retaining operation-specific
authority. Lifecycle overrides also defeat an existing server's environment.
Store discovery and PATH keep their existing contracts; see
docs/spec/provider-pane-routing.md for the ownership boundary.
"""

import os
import subprocess
import sys


GITHUB_CREDENTIALS = ("GH_CONFIG_DIR", "GH_TOKEN", "GITHUB_TOKEN",
                      "GH_ENTERPRISE_TOKEN", "GITHUB_ENTERPRISE_TOKEN")
GITHUB_ROUTING = ("STORYHOOK_GITHUB_AUTHORITY", "STORYHOOK_GITHUB_EXPECTED")


def main():
    """Retain the caller's lock here while the detached terminal boundary runs."""
    environment = os.environ.copy()
    environment.pop("STORY_WORKSPACE_LOCK_FD", None)
    pane_environment = {name: "" for name in GITHUB_CREDENTIALS + GITHUB_ROUTING}
    # A daemon may select an immutable lease. Do not replace that choice with
    # a server's stale binary or strip it like a build/test child's selector.
    pane_environment["STORY_BIN"] = environment.get("STORY_BIN", "")
    for name in pane_environment:
        environment.pop(name, None)
    arguments = sys.argv[1:]
    # Apply before the launch shell starts, without modifying existing shared
    # server/session state. Empty STORY_BIN retains the shell adapter's fallback.
    for index, argument in enumerate(arguments):
        if argument in ("new-session", "new-window", "respawn-pane"):
            overrides = [part for name, value in pane_environment.items()
                         for part in ("-e", name + "=" + value)]
            arguments[index + 1:index + 1] = overrides
            break
    # The guardian keeps inherited locks until the client exits. A new server
    # and its panes must not inherit them and outlive the dispatch handoff.
    result = subprocess.run(["tmux", *arguments], env=environment, close_fds=True)
    return result.returncode if result.returncode >= 0 else 128 - result.returncode


if __name__ == "__main__":
    sys.exit(main())
