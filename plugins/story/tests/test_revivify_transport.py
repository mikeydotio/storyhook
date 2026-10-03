"""Real RV-10 ownership through StoryHook's shipping launch paths (SH-825)."""

import io
import importlib.util
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import sys
import tarfile
import tempfile
import unittest
from concurrent.futures import ThreadPoolExecutor
from unittest.mock import patch

sys.dont_write_bytecode = True
PLUGIN = Path(__file__).resolve().parents[1]
ROOT = PLUGIN.parents[1]
sys.path.insert(0, str(ROOT / "scripts/tests"))
import load_grace  # noqa: E402

REVISION = "7f77ee8997a9e984479a910c14735852b63b5c83"


def view_program():
    """Compose the same shipping sources as the daemon's VIEW_PROGRAM."""
    return ((PLUGIN / "lib/probe_budget.py").read_text()
            + "\nprobe_run = run\nprobe_operation = operation\n"
            + (PLUGIN / "lib/tmux_server_env.py").read_text() + "\n"
            + (PLUGIN / "lib/tmux_target.py").read_text() + "\n"
            + (ROOT / "scripts/verification-view.py").read_text())


class TransportTests(unittest.TestCase):
    """Each case owns all processes and sockets it can stop or replace."""

    @classmethod
    def setUpClass(cls):
        """Export a pinned provider without installation or network dependencies."""
        cls.patience = load_grace.patience(45, load_grace.contention())
        cls.provider_temp = tempfile.TemporaryDirectory(prefix="rv825-", dir="/tmp")
        cls.addClassCleanup(cls.provider_temp.cleanup)
        cls.provider = Path(cls.provider_temp.name).resolve()
        common = subprocess.run(["git", "rev-parse", "--path-format=absolute", "--git-common-dir"],
                                cwd=ROOT, capture_output=True, text=True, check=True, timeout=cls.patience)
        checkout = os.environ.get("STORY_TEST_REVIVIFY_REPO") or str(Path(common.stdout.strip()).parent.parent / "tmux-revivify")
        exported = subprocess.run(["git", "-C", checkout, "archive", REVISION],
                                  capture_output=True, timeout=cls.patience)
        if exported.returncode:
            raise AssertionError("RV-10 test prerequisite unavailable: set STORY_TEST_REVIVIFY_REPO to a repository "
                                 f"containing {REVISION}: {exported.stderr.decode()}")
        with tarfile.open(fileobj=io.BytesIO(exported.stdout)) as archive:
            # The reviewed Git tree contains only repository-relative paths.
            for member in archive.getmembers():
                if member.name.startswith("/") or ".." in Path(member.name).parts or member.issym() or member.islnk():
                    raise AssertionError(f"unsafe provider archive entry: {member.name}")
            if hasattr(tarfile, "data_filter"):
                archive.extractall(cls.provider, filter="data")
            else:
                archive.extractall(cls.provider)
        cls.cli = str(cls.provider / "bin/revivify")

    def setUp(self):
        """Start and adopt a bare isolated server; no user config is loaded."""
        self.temp = tempfile.TemporaryDirectory(prefix="sh825-", dir="/tmp")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.env = {"HOME": str(self.root), "PATH": os.path.dirname(sys.executable) + os.pathsep + os.environ["PATH"],
                    "TMUX_TMPDIR": str(self.root), "PYTHONDONTWRITEBYTECODE": "1", "LANG": "en_US.UTF-8",
                    "STORYHOOK_VERIFIER_MIRROR": "1"}
        socket_dir = self.root / ("tmux-" + str(os.getuid()))
        socket_dir.mkdir(mode=0o700)
        self.socket = str(socket_dir / "default")
        self.tmux = shutil.which("tmux")
        self.assertIsNotNone(self.tmux)
        self.command([self.tmux, "-S", self.socket, "-f", "/dev/null", "new-session", "-d", "-s", "original", "/bin/sleep", "600"])
        self.addCleanup(self.stop)
        self.pid = int(self.command([self.tmux, "-N", "-S", self.socket, "display-message", "-p", "#{pid}"]).stdout)
        self.record = json.loads(self.command([self.cli, "server", "adopt", "--socket", self.socket,
                                              "--state-dir", str(self.root / "custom-snapshots"), "--json"]).stdout)
        self.endpoint = self.record["endpoint"]
        self.inode = os.stat(self.endpoint).st_ino
        (self.root / "logs").mkdir()
        self.reader = self.root / "reader"
        self.reader.write_text("#!/bin/sh\nexec /bin/sleep 600\n")
        self.reader.chmod(0o700)

    def command(self, argv, check=True, **kwargs):
        """Run one bounded fixture command with complete diagnostics."""
        result = subprocess.run(argv, env=kwargs.pop("env", self.env), capture_output=True,
                                text=True, timeout=self.patience, **kwargs)
        if check:
            self.assertEqual(result.returncode, 0, (argv[:3], result.stdout, result.stderr))
        return result

    def stop(self):
        """Release a stopped fixture before killing only its owned servers."""
        if hasattr(self, "pid"):
            try:
                os.kill(self.pid, signal.SIGCONT)
            except ProcessLookupError:
                pass
        for endpoint in set((self.socket, getattr(self, "endpoint", self.socket))):
            self.command([self.tmux, "-N", "-S", endpoint, "kill-server"], check=False)

    def invoke(self, path):
        """Execute the actual composed reader or provider-launch boundary."""
        if path == "view":
            return self.command([sys.executable, "-B", "-c", view_program(), "fixture",
                                 str(self.root / "logs"), str(self.reader)], check=False)
        return self.command([sys.executable, "-B", str(PLUGIN / "lib/tmux-launch.py"),
                             "new-session", "-d", "-s", "fixture", "/bin/sleep", "600"], check=False,
                            env=dict(self.env, TMUX=self.socket + ",0,0"))

    def test_displaced_public_socket_is_ignored_by_both_shipping_paths(self):
        os.unlink(self.socket)
        self.command([self.tmux, "-S", self.socket, "-f", "/dev/null", "new-session", "-d", "-s", "foreign", "/bin/sleep", "600"])
        for path in ("view", "launch"):
            with self.subTest(path=path):
                result = self.invoke(path)
                if path == "view":
                    self.assertEqual(result.returncode, 0, result.stderr)
                else:
                    # The view already allocated this exact session on the owner.
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("duplicate session", result.stderr)
        private = self.command([self.tmux, "-N", "-S", self.endpoint, "list-sessions", "-F", "#{session_name}"]).stdout
        public = self.command([self.tmux, "-N", "-S", self.socket, "list-sessions", "-F", "#{session_name}"]).stdout
        self.assertEqual(set(private.splitlines()), {"original", "fixture"})
        self.assertEqual(public.strip(), "foreign")
        self.assertEqual(os.stat(self.endpoint).st_ino, self.inode)

    def test_lifecycle_inventory_uses_real_rv10_and_retains_private_binding(self):
        sys.path.insert(0, str(PLUGIN / 'lib'))
        import agent_identity
        import tmux_client
        os.unlink(self.socket)
        self.command([self.tmux, '-S', self.socket, '-f', '/dev/null', 'new-session', '-d', '-s', 'foreign', '/bin/sleep', '600'])
        with patch.dict(os.environ, self.env, clear=True), tmux_client.operation():
            panes = agent_identity.panes(self.socket)
            self.assertEqual(len(panes), 1)
            pane = next(iter(panes.values()))
            self.assertEqual(pane['socket'], self.endpoint)
            self.assertEqual(agent_identity.pane_at(pane['pane'], pane['socket']), pane)
        inspected = self.command([sys.executable, '-B', str(PLUGIN / 'lib/tmux-env.py'), 'retained'],
                                 env=dict(self.env, TMUX=self.endpoint + ',0,0'))
        self.assertEqual(inspected.returncode, 0)
        self.assertEqual(os.stat(self.endpoint).st_ino, self.inode)
        self.assertEqual(self.command([self.tmux, '-N', '-S', self.socket, 'list-sessions', '-F', '#{session_name}']).stdout.strip(), 'foreign')

    def test_saturated_private_listener_preserves_work_and_generation(self):
        peers = []
        os.kill(self.pid, signal.SIGSTOP)
        try:
            refused = False
            for _ in range(256):
                peer = socket.socket(socket.AF_UNIX)
                peers.append(peer)
                peer.settimeout(0.01)  # A non-answer is the intentional listener saturation proof.
                try:
                    peer.connect(self.endpoint)
                except OSError:
                    refused = True
                    break
            self.assertTrue(refused, "fixture did not saturate the real listener")
            for path in ("view", "launch"):
                with self.subTest(path=path):
                    result = self.invoke(path)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("revivify", result.stderr)
                    self.assertEqual(os.stat(self.endpoint).st_ino, self.inode)
                    self.assertEqual(os.stat(self.socket).st_ino, self.inode)
        finally:
            for peer in peers:
                peer.close()
            os.kill(self.pid, signal.SIGCONT)
        report = json.loads(self.command([self.cli, "server", "inspect", "--socket", self.socket, "--json"]).stdout)
        self.assertEqual(report["generation"], self.record["generation"])
        self.assertEqual(report["identity"], self.record["identity"])
        self.assertEqual(self.command([self.tmux, "-N", "-S", self.endpoint, "list-sessions", "-F", "#{session_name}"]).stdout.strip(), "original")

    def test_missing_private_endpoint_is_not_recreated(self):
        os.unlink(self.endpoint)
        for path in ("view", "launch"):
            result = self.invoke(path)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("revivify", result.stderr)
            self.assertFalse(os.path.exists(self.endpoint))
        self.assertEqual(self.command([self.tmux, "-N", "-S", self.socket, "list-sessions", "-F", "#{session_name}"]).stdout.strip(), "original")

    def test_endpoint_disappearing_after_ensure_cannot_start_a_replacement(self):
        sys.path.insert(0, str(PLUGIN / "lib"))
        spec = importlib.util.spec_from_file_location("fixture_tmux_launch", PLUGIN / "lib/tmux-launch.py")
        launcher = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(launcher)
        run = subprocess.run
        allocations = []

        def lose_endpoint(argv, **kwargs):
            if argv[0] == "tmux" and "new-session" in argv:
                allocations.append(argv)
                os.unlink(self.endpoint)
            return run(argv, **kwargs)

        with patch.dict(os.environ, dict(self.env, TMUX=self.socket + ",0,0"), clear=True), \
                patch.object(sys, "argv", ["tmux-launch.py", "new-session", "-d", "-s", "fixture"]), \
                patch.object(launcher.subprocess, "run", side_effect=lose_endpoint):
            with launcher.probe_budget.operation():
                self.assertNotEqual(launcher.main(), 0)
        self.assertEqual(len(allocations), 1)
        self.assertEqual(allocations[0][1:4], ["-N", "-S", self.endpoint])
        self.assertFalse(os.path.exists(self.endpoint))
        self.assertEqual(self.command([self.tmux, "-N", "-S", self.socket, "list-sessions", "-F", "#{session_name}"]).stdout.strip(), "original")

    def test_concurrent_views_reuse_one_generation_and_one_reader(self):
        with ThreadPoolExecutor(max_workers=2) as pool:
            results = list(pool.map(self.invoke, ["view", "view"]))
        for result in results:
            self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads(self.command([self.cli, "server", "inspect", "--socket", self.socket, "--json"]).stdout)
        self.assertEqual(report["generation"], self.record["generation"])
        panes = self.command([self.tmux, "-N", "-S", self.endpoint, "list-panes", "-s", "-t", "=fixture", "-F", "#{pane_id}"]).stdout.splitlines()
        self.assertEqual(len(panes), 1)


if __name__ == "__main__":
    unittest.main()
