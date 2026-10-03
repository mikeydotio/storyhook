"""Execute only after the supervisor has committed this exact blocked launch."""

import os
import sys


def main():
    """EOF before the launch byte means the supervisor died; no workload may start."""
    receive, ready, guard = map(int, sys.argv[1:4])
    os.set_inheritable(guard, True)
    os.write(ready, b"R")
    os.close(ready)
    allowed = os.read(receive, 1) == b"G"
    os.close(receive)
    if not allowed:
        return 125
    os.execvpe(sys.argv[4], sys.argv[4:], os.environ)


if __name__ == "__main__":
    sys.exit(main())
