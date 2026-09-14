"""Exercise the production terminal boundary with a real server and real flocks."""

import fcntl
import os
from pathlib import Path
import select
import subprocess
import tempfile
import unittest


HELPER = Path(__file__).resolve().parents[1] / "lib/tmux-launch.py"


class DispatchWorkspaceTests(unittest.TestCase):
    """Only the caller owns a workspace after the terminal client returns."""

    def test_fresh_server_cannot_retain_direct_or_inherited_workspace_ownership(self):
        """Closing terminal descriptors preserves admission through handoff only."""
        for inherited in (False, True):
            with self.subTest(inherited=inherited), tempfile.TemporaryDirectory(
                    prefix="story-dispatch-lock-", dir="/tmp") as directory:
                root = Path(directory)
                lock = root / "workspace.lock"
                socket = root / "server.sock"
                script = r"""set -euo pipefail
exec 9>>"$1"
python3 -c 'import fcntl; fcntl.flock(9, fcntl.LOCK_EX | fcntl.LOCK_NB)'
if [ "$4" = inherited ]; then
  exec 11<&9
  export STORY_WORKSPACE_LOCK_FD=11
fi
python3 "$3" -S "$2" -f /dev/null new-session -d -s fixture 'sleep 120'
printf 'ready\n'
read -r release
"""
                holder = subprocess.Popen(
                    ["bash", "-c", script, "fixture", str(lock), str(socket),
                     str(HELPER), "inherited" if inherited else "direct"],
                    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                    text=True)
                try:
                    ready, _, _ = select.select([holder.stdout], [], [], 10)
                    self.assertTrue(ready, "terminal startup did not finish")
                    self.assertEqual(holder.stdout.readline(), "ready\n")
                    with lock.open("r+") as probe:
                        with self.assertRaises(BlockingIOError):
                            fcntl.flock(probe, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    _, error = holder.communicate("release\n", timeout=10)
                    self.assertEqual(holder.returncode, 0, error)
                    with lock.open("r+") as probe:
                        fcntl.flock(probe, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    pane_pid = subprocess.check_output(
                        ["tmux", "-S", str(socket), "display-message", "-p", "#{pane_pid}"],
                        text=True, timeout=5)
                    os.kill(int(pane_pid), 0)
                    environment = subprocess.check_output(
                        ["tmux", "-S", str(socket), "show-environment", "-g"],
                        text=True, timeout=5)
                    self.assertNotIn("STORY_WORKSPACE_LOCK_FD=", environment)
                finally:
                    if holder.poll() is None:
                        holder.terminate()
                        holder.communicate(timeout=10)
                    if socket.exists():
                        subprocess.run(["tmux", "-S", str(socket), "kill-server"],
                                       check=True, capture_output=True, timeout=10)


if __name__ == "__main__":
    unittest.main()
