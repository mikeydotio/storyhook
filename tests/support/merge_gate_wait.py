"""Blocking leaf fixtures for merge_gate; owned by the enclosing ChildGuard."""

import argparse
import os
from pathlib import Path
import signal
import socket
import time


def publish(marker, pid):
    """Readiness means initialization finished and a complete PID is visible."""
    temporary = marker.with_suffix(".tmp")
    temporary.write_text(f"{pid}\n")
    temporary.replace(marker)


def signal_wait(marker, delay):
    """Remain a leaf, including while the deliberate cancellation delay runs."""
    def delayed_exit(signum, _frame):
        time.sleep(delay)
        raise SystemExit(128 + signum)

    for signum in (signal.SIGHUP, signal.SIGINT, signal.SIGTERM):
        signal.signal(signum, delayed_exit if delay else signal.SIG_DFL)
    publish(marker, os.getpid())
    while True:
        signal.pause()


def barrier_wait(marker, address, pid):
    """A disconnected controller must never look like successful restoration."""
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.connect(address)
        publish(marker, pid)
        message = connection.recv(1)
        if message != b"1":
            raise RuntimeError(f"release barrier {address}: expected b'1', received {message!r}")


def main():
    """Run a signal waiter or an explicitly released restoration barrier."""
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_subparsers(dest="mode", required=True)
    signals = modes.add_parser("signal")
    signals.add_argument("marker", type=Path)
    signals.add_argument("delay", type=float)
    barrier = modes.add_parser("barrier")
    barrier.add_argument("marker", type=Path)
    barrier.add_argument("address")
    barrier.add_argument("pid", type=int)
    args = parser.parse_args()
    if args.mode == "signal":
        signal_wait(args.marker, args.delay)
    else:
        barrier_wait(args.marker, args.address, args.pid)


if __name__ == "__main__":
    main()
