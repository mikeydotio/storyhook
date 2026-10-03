"""Expose the shared ownership resolver to shell operations as checked JSON."""

import json
import os
import subprocess
import sys

sys.dont_write_bytecode = True
import probe_budget
from tmux_target import logical_socket, resolve_target, require_current_selector
from tmux_server_env import client_environment


def main(arguments):
    """Inspect without startup, or ensure before dispatch."""
    if not arguments or arguments[0] not in ('select', 'inspect', 'ensure') or len(arguments) > 2:
        raise RuntimeError('expected select|inspect|ensure [absolute-socket]')
    mode = arguments[0]
    socket = logical_socket(arguments[1] if len(arguments) == 2 else None, os.environ)
    if mode == 'select':
        # Selection is a hint for native resource precedence, not ownership.
        print(json.dumps(dict(socket=socket)))
        return
    target = resolve_target(socket, os.environ, probe_budget.run,
                            client_environment(os.environ), ensure=mode == 'ensure')
    require_current_selector(target, socket)
    print(json.dumps(target))


if __name__ == '__main__':
    try:
        with probe_budget.operation():
            main(sys.argv[1:])
    except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
        print(f'tmux-target: {error}', file=sys.stderr)
        sys.exit(1)
