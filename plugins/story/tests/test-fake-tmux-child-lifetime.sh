#!/usr/bin/env bash
# Both pane shapes must use the caller's lifetime, including load grace.
source "$(dirname "$0")/lib.sh"

if ! python3 - "$TESTS_DIR" <<'PY'
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time

tests = Path(sys.argv[1])
sys.path.insert(0, str(tests.parents[2] / "scripts/tests"))
from load_grace import PATIENCE_CEILING, Patience, contention, multiplier, patience


def wait_for(probe):
    """Wait for fixture state with the shared contention allowance."""
    ratio = contention()
    base = 5
    budget = Patience(patience(base, ratio), multiplier(ratio, float("inf")),
                      PATIENCE_CEILING, time.monotonic())
    while True:
        if probe():
            return
        if budget.expired(time.monotonic()):
            raise AssertionError("fake pane did not reach the required state")
        time.sleep(0.01)


def alive(pid):
    """Distinguish a running placeholder from its unreaped exit record."""
    result = subprocess.run(["ps", "-o", "stat=", "-p", str(pid)],
                            capture_output=True, text=True, check=False)
    return result.returncode == 0 and not result.stdout.strip().startswith("Z")


for lifetime in ("0.25", "91", None):
    with tempfile.TemporaryDirectory(prefix="story-test-pane-", dir="/tmp") as root:
        state = Path(root)
        binary = state / "bin"
        binary.mkdir()
        # A gated sleep double observes the actual argv without racing expiry.
        sleep = binary / "sleep"
        sleep.write_text('#!/bin/sh\nprintf "%s" "$1" > "$FAKE_TMUX_STATE/lifetime"\n'
                         'while [ ! -f "$FAKE_TMUX_STATE/release" ]; do /bin/sleep 0.01; done\n')
        sleep.chmod(0o755)
        env = dict(os.environ, FAKE_TMUX_STATE=root, FAKE_TMUX_PANE_CHILD="1",
                   PATH=f"{binary}:{os.environ['PATH']}")
        env.pop("FAKE_TMUX_PANE_LIFETIME", None)
        if lifetime is not None:
            env["FAKE_TMUX_PANE_LIFETIME"] = lifetime
        try:
            subprocess.run([str(tests / "fakes/tmux"), "new-window", "-n", "TST-1",
                            "-c", root, "claude", ";", "set-window-option",
                            "-t", "@1", "remain-on-exit", "on"], env=env, check=True,
                           stdout=subprocess.DEVNULL)
            wait_for(lambda: (state / "lifetime").exists())
            parent = int((state / "pane_pid").read_text())
            child = int((state / "child_pid").read_text())
            assert alive(parent) and alive(child), "both processes must stay live"
            actual_parent = subprocess.check_output(["ps", "-o", "ppid=", "-p", str(child)])
            assert int(actual_parent) == parent, "the pane must own the child"
            assert (state / "lifetime").read_text() == (lifetime or "30"), lifetime
            (state / "release").touch()
            wait_for(lambda: not alive(parent))
            assert not alive(child), "the parent must reap its completed child"
        finally:
            (state / "release").touch()
            for name in ("child_pid", "pane_pid"):
                if (state / name).exists():
                    try:
                        os.kill(int((state / name).read_text()), signal.SIGTERM)
                    except ProcessLookupError:
                        pass
PY
then
  fail_test "child panes must use one lifetime and reap their child"
fi

finish
