"""Exercise the production terminal boundary with a real server and real flocks."""

import fcntl
import os
from pathlib import Path
import select
import time
import json
import shlex
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from test_tmux_routing import TmuxRoutingTests


HELPER = Path(__file__).resolve().parents[1] / "lib/tmux-launch.py"


def environment_snapshot_program(output):
    """Publish the fixture result only after its complete JSON file is closed."""
    return f"""import json, os, tempfile
output = {str(output)!r}
with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8",
        dir=os.path.dirname(output), prefix=os.path.basename(output) + ".",
        delete=False) as staged:
    json.dump(dict(os.environ), staged)
    temporary = staged.name
os.replace(temporary, output)
"""


class DispatchWorkspaceTests(unittest.TestCase):
    """Only the caller owns a workspace after the terminal client returns."""

    def test_credentials_are_empty_in_new_and_existing_server_panes(self):
        """The real tmux environment merge cannot restore server credentials."""
        names = ("GH_CONFIG_DIR", "GH_TOKEN", "GITHUB_TOKEN",
                 "GH_ENTERPRISE_TOKEN", "GITHUB_ENTERPRISE_TOKEN")
        for existing in (False, True):
            with self.subTest(existing=existing), tempfile.TemporaryDirectory(dir="/tmp") as d:
                root = Path(d)
                socket = root / "tmux.sock"
                env = dict(os.environ, **{name: "secret-fixture" for name in names})
                base = ["tmux", "-S", str(socket), "-f", "/dev/null"]
                try:
                    if existing:
                        subprocess.run(base + ["new-session", "-d", "-s", "fixture", "sleep 120"],
                                       env=env, check=True)
                    for command in (("new-window", "respawn-pane") if existing else ("new-session",)):
                        output = root / (command + ".json")
                        program = environment_snapshot_program(output)
                        launch = "python3 -c " + shlex.quote(program) + "; sleep 120"
                        args = (["-k", "-t", "fixture:"] if command == "respawn-pane" else
                                ["-d", "-t", "fixture:"] if command == "new-window" else
                                ["-d", "-s", "fixture"])
                        subprocess.run(["python3", str(HELPER), *base[1:], command, *args, launch],
                                       env=env, check=True)
                        deadline = time.monotonic() + 10
                        while not output.exists() and time.monotonic() < deadline:
                            time.sleep(0.02)
                        observed = json.loads(output.read_text())
                        for name in names:
                            self.assertFalse(observed.get(name), (existing, command, name))
                    if not existing:
                        global_env = subprocess.check_output(base + ["show-environment", "-g"], text=True)
                        self.assertNotIn("secret-fixture", global_env)
                finally:
                    if socket.exists():
                        subprocess.run(base + ["kill-server"], check=True, capture_output=True)

    def test_environment_snapshot_is_complete_and_closed_before_final_visibility(self):
        """Observe the real producer precisely at publication, without sleeps."""
        with tempfile.TemporaryDirectory(prefix="snapshot-publication-", dir="/tmp") as directory:
            output = Path(directory) / "environment.json"
            expected = {"SNAPSHOT_FIXTURE": "complete\nJSON \u2603 " + "x" * 32768}
            opened = []
            real_temporary_file = tempfile.NamedTemporaryFile
            real_replace = os.replace

            def stage(*args, **kwargs):
                handle = real_temporary_file(*args, **kwargs)
                opened.append(handle)
                return handle

            def publish(source, destination):
                self.assertEqual(Path(destination), output)
                self.assertFalse(output.exists(), "partial JSON became visible before publication")
                self.assertEqual(len(opened), 1)
                self.assertTrue(opened[0].closed, "publication must follow producer close")
                self.assertEqual(Path(source).parent, output.parent)
                self.assertEqual(json.loads(Path(source).read_text()), expected)
                real_replace(source, destination)

            with patch.dict(os.environ, expected, clear=True), \
                    patch.object(tempfile, "NamedTemporaryFile", side_effect=stage), \
                    patch.object(os, "replace", side_effect=publish) as replacement:
                exec(environment_snapshot_program(output), {})
                replacement.assert_called_once()

            self.assertEqual(json.loads(output.read_text()), expected)
            self.assertEqual(list(Path(directory).iterdir()), [output])

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
