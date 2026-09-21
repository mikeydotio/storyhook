"""Observe routing through the production launcher and a private real tmux."""

import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile
import time
import unittest


PLUGIN = Path(__file__).resolve().parents[1]
SELECTORS = ("STORY_BIN", "STORYHOOK_GITHUB_AUTHORITY", "STORYHOOK_GITHUB_EXPECTED")

# The probe exercises the real binary and shell adapter; only observation is
# test-owned. Publish the receipt atomically so readers cannot see partial JSON.
PROBE = r'''
import json, os, pathlib, subprocess, sys
output, adapter = sys.argv[1:]
def run(args):
    try:
        result = subprocess.run(args, text=True, capture_output=True, timeout=15)
    except FileNotFoundError as error:
        return {"code": 127, "out": "", "err": str(error)}
    return {"code": result.returncode, "out": result.stdout, "err": result.stderr}
receipt = {"environment": {name: os.environ.get(name) for name in
           ("STORY_BIN", "STORYHOOK_GITHUB_AUTHORITY", "STORYHOOK_GITHUB_EXPECTED",
            "STORYHOOK_STORE_PATH", "XDG_STATE_HOME", "SH738_MARKER")}}
receipt["story"] = run([os.environ.get("STORY_BIN") or "story", "show", "RTE-1", "--json"])
receipt["github"] = run(["bash", "-c", 'source "$1"; if github_begin; then printf "%s" "$STORYHOOK_GITHUB_EXPECTED"; else printf "%s" "$GITHUB_ACCESS_ERROR" >&2; exit 1; fi', "probe", adapter])
temporary = pathlib.Path(output + ".tmp")
temporary.write_text(json.dumps(receipt))
temporary.replace(output)
'''


class TmuxRoutingTests(unittest.TestCase):
    """Binary ownership survives, while each pane owns its GitHub operation."""

    def test_real_pane_routing_matrix(self):
        """Fresh/retained servers agree for set, empty and absent binary selectors."""
        self.assertTrue(os.environ.get("STORYHOOK_TEST_HOME"), "run via test-dispatch-workspace.sh")
        with tempfile.TemporaryDirectory(prefix="story-routing-", dir="/tmp") as directory:
            root = Path(directory)
            repo = root / "pane checkout"
            foreign = root / "foreign checkout"
            doctor = root / "doctor checkout"
            for checkout, origin in ((repo, "https://github.example/acme/pane"),
                                     (foreign, "https://github.com/foreign/parent"),
                                     (doctor, None)):
                checkout.mkdir()
                self.run_command(["git", "init", "-q", str(checkout)])
                if origin:
                    self.run_command(["git", "-C", str(checkout), "remote", "add", "origin", origin])
            self.run_command(["story", "project", "new", "--prefix", "RTE"], cwd=repo)
            self.run_command(["story", "new", "Pane-owned story"], cwd=repo)
            shutil.copyfile(repo / ".storyhook.toml", doctor / ".storyhook.toml")
            # Exec the harness's lease at its established path: a symlink alias
            # changes executable identity and legitimately trips the daemon guard.
            leased = root / "leased story"
            leased.write_text("#!/bin/sh\nexec " + shlex.quote(shutil.which("story")) + ' "$@"\n')
            leased.chmod(0o755)
            probe = root / "probe.py"
            probe.write_text(PROBE)
            for mode in ("fresh", "new-session", "new-window", "respawn-pane", "doctor"):
                for binary in (None, "", str(leased)):
                    with self.subTest(mode=mode, binary=binary):
                        self.observe(root, repo, doctor, foreign, probe, mode, binary)

    def run_command(self, args, **kwargs):
        """Run a bounded fixture command and retain its diagnostic on failure."""
        result = subprocess.run(args, capture_output=True, text=True, timeout=20, **kwargs)
        self.assertEqual(result.returncode, 0, (args, result.stdout, result.stderr))
        return result.stdout

    def observe(self, root, repo, doctor, foreign, probe, mode, binary):
        """Compare caller, retained server, and actual pane routing independently."""
        socket = root / "tmux.sock"
        base = ["tmux", "-S", str(socket), "-f", "/dev/null"]
        env = dict(os.environ, STORYHOOK_GITHUB_AUTHORITY=str(foreign),
                   STORYHOOK_GITHUB_EXPECTED="github.com/foreign/parent")
        env.pop("STORY_BIN", None)
        if binary is not None:
            env["STORY_BIN"] = binary
        parent = env.copy()
        receipt = root / "receipt.json"
        receipt.unlink(missing_ok=True)
        command = "new-session" if mode == "fresh" else "new-window" if mode == "doctor" else mode
        args = (["-d", "-s", "provider"] if command == "new-session" else
                ["-k", "-t", "fixture:worker.0"] if command == "respawn-pane" else
                ["-d", "-t", "fixture:"])
        cwd = doctor if mode == "doctor" else repo
        launch = shlex.join(["python3", str(probe), str(receipt), str(PLUGIN / "lib/github-access.sh")])
        try:
            if mode != "fresh":
                retained = dict(env, STORY_BIN="/stale/global/story")
                self.run_command(base + ["new-session", "-d", "-s", "fixture", "-n", "unrelated", "sleep 120"], env=retained)
                self.run_command(base + ["new-window", "-d", "-t", "fixture:", "-n", "worker", "sleep 120"])
                for name, value in zip(SELECTORS, ("/stale/session/story", str(foreign), "github.com/stale/session")):
                    self.run_command(base + ["set-environment", "-t", "fixture", name, value])
                global_before = self.run_command(base + ["show-environment", "-g"])
                session_before = self.run_command(base + ["show-environment", "-t", "fixture"])
                unrelated = self.run_command(base + ["display-message", "-p", "-t", "fixture:unrelated.0", "#{pane_pid}"])
            output = self.run_command(["python3", str(PLUGIN / "lib/tmux-launch.py"), *base[1:], command,
                                       *args, "-c", str(cwd), "-e", "SH738_MARKER=preserved", launch + "; sleep 120",
                                       ";", "display-message", "-p", "SH738_CHAIN"], env=env)
            self.assertEqual(output.strip(), "SH738_CHAIN")
            deadline = time.monotonic() + 20
            while not receipt.exists() and time.monotonic() < deadline:
                time.sleep(0.02)
            self.assertTrue(receipt.exists(), "pane probe did not publish its receipt")
            observed = json.loads(receipt.read_text())
            self.assertEqual(observed["environment"]["SH738_MARKER"], "preserved")
            # Subtests keep the routing results visible even when an environment
            # assertion fails, distinguishing a refusal from an incorrect target.
            with self.subTest(check="binary"):
                self.assertEqual(observed["environment"]["STORY_BIN"], binary or "")
            for name in SELECTORS[1:]:
                with self.subTest(check=name):
                    self.assertEqual(observed["environment"][name], "")
            with self.subTest(check="story"):
                self.assertEqual(observed["story"]["code"], 0, observed)
                story = json.loads(observed["story"]["out"])["story"]["story"]
                self.assertEqual((story["id"], story["title"]), ("RTE-1", "Pane-owned story"))
            with self.subTest(check="github"):
                github = observed["github"]
                if mode == "doctor":
                    self.assertNotEqual(github["code"], 0)
                    self.assertIn("origin", github["err"])
                else:
                    self.assertEqual(github["code"], 0, github)
                    self.assertEqual(github["out"], "github.example/acme/pane")
            for name in ("STORYHOOK_STORE_PATH", "XDG_STATE_HOME"):
                self.assertEqual(observed["environment"][name], env[name])
            self.assertEqual(env, parent)
            if mode == "fresh":
                global_after = self.run_command(base + ["show-environment", "-g"])
                for name in SELECTORS:
                    with self.subTest(check="fresh-global", name=name):
                        self.assertFalse(any(line.startswith(name + "=") for line in global_after.splitlines()))
            else:
                self.assertEqual(self.run_command(base + ["show-environment", "-g"]), global_before)
                self.assertEqual(self.run_command(base + ["show-environment", "-t", "fixture"]), session_before)
                self.assertEqual(self.run_command(base + ["display-message", "-p", "-t", "fixture:unrelated.0", "#{pane_pid}"]), unrelated)
                os.kill(int(unrelated), 0)
            refused = subprocess.run(["python3", str(PLUGIN / "lib/tmux-launch.py"), *base[1:],
                                      "respawn-pane", "-t", "missing-session:", "true"],
                                     env=env, capture_output=True, text=True, timeout=10)
            self.assertEqual(refused.returncode, 1, refused.stderr)
            self.assertIn("missing-session", refused.stderr)
        finally:
            if socket.exists():
                self.run_command(base + ["kill-server"])
