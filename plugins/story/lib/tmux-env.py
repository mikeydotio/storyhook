"""Command-line entry point for the tmux server environment policy (SH-758).

  tmux-env.py scrub-session <session>  Clean one storyhook-owned session.
  tmux-env.py retained                 Report session state the server holds.

Both print one JSON object. Neither command can start a server, so each tmux
call keeps the caller's routing and test-harness environment unchanged.
"""

import json
import os
import subprocess
import sys

# An installed plugin directory is not this process's to write into.
sys.dont_write_bytecode = True
import probe_budget
import tmux_server_env


def run(*arguments):
    """Run one tmux command and return stdout, failing loudly with its diagnostic.

    The helper budget bounds each call, so a wedged server cannot hold a dispatch.
    """
    environment = os.environ.copy()
    environment.pop("STORY_WORKSPACE_LOCK_FD", None)
    result = probe_budget.run(["tmux", *arguments], env=environment, capture_output=True, text=True,
                              close_fds=True)
    if result.returncode:
        raise RuntimeError(f"tmux {' '.join(arguments)}: {result.stderr.strip()} (exit {result.returncode})")
    return result.stdout


def main(argv):
    """Dispatch one policy command; see tmux_server_env for the contract."""
    if len(argv) == 2 and argv[0] == "scrub-session":
        report = tmux_server_env.scrub_owned_session(run, argv[1], os.environ)
    elif argv == ["retained"]:
        variables = tmux_server_env.parse_environment(run("show-environment", "-g"))
        report = {"names": tmux_server_env.retained_names(variables)}
    else:
        print("usage: tmux-env.py scrub-session <session> | retained", file=sys.stderr)
        return 2
    print(json.dumps(report))
    return 0


if __name__ == "__main__":
    try:
        # One invocation is one operation: its tmux calls share one budget.
        with probe_budget.operation():
            status = main(sys.argv[1:])
        sys.exit(status)
    except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
        print(f"tmux-env: {error}", file=sys.stderr)
        sys.exit(1)
