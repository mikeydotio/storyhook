"""Execute only after the supervisor has committed this exact blocked launch."""

import os
import sys


def main():
    """EOF before the launch byte means the supervisor died; no workload may start."""
    receive, ready, guard, error = map(int, sys.argv[1:5])
    os.set_inheritable(guard, True)
    os.set_inheritable(error, False)
    os.write(ready, b"R")
    os.close(ready)
    allowed = os.read(receive, 1) == b"G"
    os.close(receive)
    if not allowed:
        return 125
    try:
        os.execvpe(sys.argv[5], sys.argv[5:], os.environ)
    except OSError as failure:
        os.write(error, str(failure).encode()[:4096])
        return 125


if __name__ == "__main__":
    sys.exit(main())
