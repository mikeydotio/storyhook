"""Give provider panes their caller's selectors, but no parent GitHub operation.

The client scrub prevents a new server from retaining operation-specific
authority. Lifecycle overrides also defeat an existing server's environment.
A client that starts a server passes only the tmux_server_env allowlist, so
the server's global environment never describes this one process (SH-758).
See docs/spec/provider-pane-routing.md for the ownership boundary.
"""

import os
import subprocess
import sys

# An installed plugin directory is not this process's to write into.
sys.dont_write_bytecode = True
import probe_budget
from tmux_server_env import (GITHUB_CREDENTIALS, GITHUB_ROUTING, PANE_SELECTORS,
                             client_environment, reports_no_server)

PANE_COMMANDS = ("new-session", "new-window", "respawn-pane")


def server_answers(prefix, environment):
    """Return whether a server answers at the socket `prefix` selects.

    Only tmux's documented absent-server diagnostics mean "no server". Any
    other failure is surfaced, because guessing would either start a server
    with this process's environment or refuse a launch that could succeed.
    """
    # The probe is bounded by the helper budget; the launch keeps tmux's own behaviour.
    probe = probe_budget.run(["tmux", *prefix, "list-sessions", "-F", "#{session_id}"], env=environment,
                             capture_output=True, text=True, close_fds=True)
    if probe.returncode == 0:
        return True
    if reports_no_server(probe.stderr):
        return False
    raise RuntimeError(f"tmux server probe failed: {probe.stderr.strip()} (exit {probe.returncode})")


def main():
    """Retain the caller's lock here while the detached terminal boundary runs."""
    environment = os.environ.copy()
    environment.pop("STORY_WORKSPACE_LOCK_FD", None)
    pane_environment = {name: "" for name in GITHUB_CREDENTIALS + GITHUB_ROUTING}
    # A daemon may select an immutable lease and a store. Do not replace that
    # choice with a server's stale value or strip it like a build/test child's
    # selector; empty keeps each reader's own fallback (`${STORY_BIN:-story}`).
    for name in ("STORY_BIN",) + PANE_SELECTORS:
        pane_environment[name] = environment.get(name, "")
    # Store selectors stay in the client: an existing server ignores its
    # client's environment for new panes, and a starting one is allowlisted.
    for name in GITHUB_CREDENTIALS + GITHUB_ROUTING + ("STORY_BIN",):
        environment.pop(name, None)
    arguments = sys.argv[1:]
    # Apply before the launch shell starts, without modifying existing shared
    # server/session state. Empty STORY_BIN retains the shell adapter's fallback.
    for index, argument in enumerate(arguments):
        if argument in PANE_COMMANDS:
            overrides = [part for name, value in pane_environment.items()
                         for part in ("-e", name + "=" + value)]
            arguments[index + 1:index + 1] = overrides
            if argument == "new-session" and not server_answers(arguments[:index], environment):
                environment = client_environment(os.environ)
            break
    # The guardian keeps inherited locks until the client exits. A new server
    # and its panes must not inherit them and outlive the dispatch handoff.
    result = subprocess.run(["tmux", *arguments], env=environment, close_fds=True)
    return result.returncode if result.returncode >= 0 else 128 - result.returncode


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
        print(f"tmux-launch: {error}", file=sys.stderr)
        sys.exit(1)
