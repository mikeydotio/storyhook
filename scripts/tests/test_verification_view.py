"""Exercise the shipping reader reconciler on an owned private tmux server."""

import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "verification-view.py"
DEADLINE = 15  # Includes private server startup and loaded macOS PTY allocation.


class ViewTests(unittest.TestCase):
    """Every fixture owns its foreground server and destroys it in cleanup."""

    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="sh748-view-", dir="/tmp"))
        self.addCleanup(shutil.rmtree, self.root)
        self.socket = self.root / "tmux.sock"
        self.env = dict(os.environ, HOME=str(self.root), STORYHOOK_VERIFIER_MIRROR="1")
        for key in ("TMUX", "TMUX_PANE", "STORYHOOK_ACTIVITY_LOG_DIR", "STORYHOOK_ACTIVITY_CONTEXT"):
            self.env.pop(key, None)
        tmux = shutil.which("tmux")
        self.assertIsNotNone(tmux)
        self.tmux_argv = [tmux, "-S", str(self.socket), "-f", "/dev/null"]
        if self._testMethodName == "test_native_allocation_failure_preserves_server_and_other_reader":
            self.marker = self.root / "fail-next-allocation"
            source = self.root / "forkpty.c"
            library = self.root / "forkpty.dylib"
            source.write_text('''#include <util.h>
#include <errno.h>
#include <stdlib.h>
#include <unistd.h>
static int fail_once(int *m, char *n, struct termios *t, struct winsize *w) {
    const char *marker = getenv("STORY_TEST_PTY_FAILURE");
    if (marker != NULL && unlink(marker) == 0) { errno = ENXIO; return -1; }
    return forkpty(m, n, t, w);
}
__attribute__((used)) static struct { const void *replacement; const void *original; }
interpose[] __attribute__((section("__DATA,__interpose"))) = {
    { (const void *)fail_once, (const void *)forkpty }
};
''')
            subprocess.run(["clang", "-dynamiclib", "-Wall", "-Werror", str(source), "-o", str(library)],
                           timeout=DEADLINE, check=True, capture_output=True)
            self.env.update(DYLD_INSERT_LIBRARIES=str(library), STORY_TEST_PTY_FAILURE=str(self.marker))
        server = subprocess.Popen(self.tmux_argv + ["-D"], env=self.env,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.addCleanup(self.stop_server, server)
        end = time.monotonic() + DEADLINE
        while not self.socket.exists():
            self.assertIsNone(server.poll())
            self.assertLess(time.monotonic(), end)
            time.sleep(.02)
        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        import shlex
        wrapper = bin_dir / "tmux"
        wrapper.write_text("#!/bin/sh\nexec " + shlex.join(self.tmux_argv) + ' "$@"\n')
        wrapper.chmod(0o700)
        self.env["PATH"] = str(bin_dir) + ":" + self.env["PATH"]
        self.reader = self.root / "reader with spaces"
        self.reader.write_text('#!/bin/sh\nprintf "READER %s\\n" "$*"\nexec sleep 2147483647\n')
        self.reader.chmod(0o700)

    def stop_server(self, server):
        """Reap the owned server even when an assertion or client fails."""
        rows = self.tmux("list-panes", "-a", "-F", "#{pane_pid}:#{pane_dead}", check=False)
        readers = [(pid, self.process_start(pid)) for pid, dead in
                   (row.split(":") for row in rows.splitlines()) if dead == "0"]
        try:
            self.tmux("kill-server", check=False)
        finally:
            if server.poll() is None:
                server.terminate()
            try:
                server.wait(timeout=DEADLINE)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait(timeout=DEADLINE)
        end = time.monotonic() + DEADLINE
        while any(token and self.process_start(pid) == token for pid, token in readers):
            self.assertLess(time.monotonic(), end, "owned pane reader survived private-server cleanup")
            time.sleep(.02)

    def process_start(self, pid):
        """Pair a PID with native start identity to exclude reuse during cleanup."""
        return subprocess.run(["ps", "-p", pid, "-o", "lstart="], capture_output=True,
                              text=True, timeout=DEADLINE).stdout.strip()

    def tmux(self, *args, check=True):
        """Bound every private control operation."""
        return subprocess.run(self.tmux_argv + list(args), env=self.env, capture_output=True,
                              text=True, timeout=DEADLINE, check=check).stdout.strip()

    def reconcile(self, project="one", check=True):
        """Run the production helper with literal hostile-path arguments."""
        directory = self.root / project / "logs with spaces ' $(inert)"
        return subprocess.run(["python3", str(SCRIPT), project, str(directory), str(self.reader)],
                              env=self.env, capture_output=True, text=True,
                              timeout=DEADLINE, check=check)

    def identity(self, project="one"):
        """Return immutable reader identity rather than a reusable index."""
        identity = self.tmux("display-message", "-p", "-t", f"={project}:=verification",
                         "#{window_id}|#{pane_id}|#{pane_pid}")
        self.assertTrue(identity.startswith("@"), "project verification window is absent")
        return identity

    def test_disabled_mirror_does_not_even_probe_tmux(self):
        wrapper = self.root / "bin/tmux"
        wrapper.write_text('#!/bin/sh\nprintf called > "$HOME/called"\nexit 99\n')
        self.env["STORYHOOK_VERIFIER_MIRROR"] = "0"
        self.reconcile()
        self.assertFalse((self.root / "called").exists())
        self.assertFalse((self.root / "one").exists())

    def test_interrupted_owned_allocation_is_reaped_without_replacing_healthy_view(self):
        self.reconcile()
        first = self.identity()
        owner = self.tmux("show-option", "-w", "-v", "-t", first.split("|")[0], "@storyhook-journal")
        orphan = self.tmux("new-window", "-d", "-P", "-F", "#{window_id}", "-t", "=one:",
                           "-n", ".verification-interrupted", "sleep", "2147483647")
        self.tmux("set-option", "-w", "-t", orphan, "@storyhook-journal", owner)
        self.reconcile()
        self.assertEqual(first, self.identity())
        self.assertEqual(self.tmux("list-windows", "-t", "=one", "-F", "#{window_name}"), "verification")

    def test_concurrent_creation_and_wrong_reader_replacement(self):
        from concurrent.futures import ThreadPoolExecutor
        with ThreadPoolExecutor(max_workers=4) as pool:
            list(pool.map(lambda _: self.reconcile(), range(8)))
        first = self.identity()
        self.assertEqual(self.tmux("list-windows", "-a", "-F", "#{window_name}"), "verification")
        # Replace only the identity marker: reconciliation must not accept an
        # otherwise live pane as the recorded reader.
        self.tmux("set-option", "-w", "-t", first.split("|")[0], "@storyhook-reader", "%999:0")
        self.reconcile()
        self.assertNotEqual(first, self.identity())

    def test_tmux_error_preserves_owned_reader_and_next_attempt_recovers(self):
        self.reconcile()
        first = self.identity()
        self.tmux("set-option", "-w", "-t", first.split("|")[0], "@storyhook-reader", "%999:0")
        wrapper = self.root / "bin/tmux"
        original = wrapper.read_text()
        wrapper.write_text(original.replace("exec ", 'if [ "$1" = new-window ]; then echo "allocation failed" >&2; exit 1; fi\nexec ', 1))
        result = self.reconcile(check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("allocation failed", result.stderr)
        self.assertEqual(first, self.identity())
        wrapper.write_text(original)
        self.reconcile()
        self.assertNotEqual(first, self.identity())

    @unittest.skipUnless(sys.platform == "darwin", "native macOS forkpty regression")
    def test_native_allocation_failure_preserves_server_and_other_reader(self):
        self.reconcile()
        self.reconcile("control")
        first, control = self.identity(), self.identity("control")
        self.tmux("set-option", "-w", "-t", first.split("|")[0], "@storyhook-reader", "%999:0")
        self.marker.touch()
        result = self.reconcile(check=False)
        self.assertFalse(self.marker.exists(), "native failure was not exercised")
        self.assertIn("Device not configured", result.stderr)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(first, self.identity())
        self.reconcile()
        self.assertNotEqual(first, self.identity())
        self.assertEqual(control, self.identity("control"))

    def test_assertion_failure_reaps_private_readers_without_touching_control(self):
        self.reconcile()
        control = self.identity()

        class FailingFixture(ViewTests):
            def runTest(inner):
                inner.reconcile()
                inner.fail("deliberate fixture assertion")

        fixture = FailingFixture()
        result = unittest.TestResult()
        fixture.run(result)
        self.assertEqual(len(result.failures), 1)
        self.assertEqual(result.errors, [])
        self.assertFalse(fixture.root.exists())
        self.assertEqual(control, self.identity())

    @unittest.skipUnless(os.environ.get("STORY_VIEW_TEST_BINARY"), "run via cargo test --test verify_window for production daemon")
    def test_production_daemon_restores_project_view_and_recovers_it_while_idle(self):
        import datetime
        import json
        binary = os.environ["STORY_VIEW_TEST_BINARY"]
        project = Path(os.environ["STORY_VIEW_TEST_PROJECT"])
        slug = os.environ["STORY_VIEW_TEST_SLUG"]
        directory = project / ".storyhook/logs"
        directory.mkdir(parents=True, exist_ok=True)
        now = datetime.datetime.now(datetime.timezone.utc)
        path = directory / (now.strftime("%Y-%m-%d") + ".jsonl")
        row = dict(at=now.isoformat(), level="INFO", source="fixture-helper", stream="stderr",
                   pid=1, context=f"project={slug} VIEW-1 attempt=live", message="LIVE_PROJECT_OUTPUT")
        path.write_text(json.dumps(row) + "\n")
        diagnostics = self.root / "daemon.err"
        with diagnostics.open("wb") as error:
            daemon = subprocess.Popen([binary, "--store-path", os.environ["STORY_VIEW_TEST_STORE"],
                                       "daemon", "--serve", "--port", "0"], cwd=project, env=self.env,
                                      stdout=subprocess.DEVNULL, stderr=error)
        self.addCleanup(self.stop_daemon, daemon)

        def wait_for_view(previous=None):
            end = time.monotonic() + DEADLINE
            while True:
                self.assertIsNone(daemon.poll(), diagnostics.read_text())
                identity = self.tmux("display-message", "-p", "-t", f"={slug}:=verification",
                                     "#{window_id}|#{pane_id}|#{pane_pid}", check=False)
                text = self.tmux("capture-pane", "-p", "-t", f"={slug}:=verification", check=False)
                if identity.startswith("@") and identity != previous and "LIVE_PROJECT_OUTPUT" in text:
                    self.assertIn("fixture-helper:stderr", text)
                    self.assertIn("VIEW-1 attempt=live", text)
                    return identity
                self.assertLess(time.monotonic(), end, diagnostics.read_text())
                time.sleep(.05)

        first = wait_for_view()
        self.tmux("kill-window", "-t", first.split("|")[0])
        second = wait_for_view(first)
        self.assertNotEqual(first, second)
        self.stop_daemon(daemon)
        # SH-761: at least two successful reconciles ran (startup and the
        # repair). The supervisor must not narrate them into the journal it
        # keeps a window on. A reconcile that loses a race with the kill above
        # may still fail once; that is journaled at ERROR/WARN by design.
        records = [json.loads(line) for line in path.read_text().splitlines() if line.strip()]
        narration = [row for row in records
                     if " reader" in row["context"] and row["level"] == "INFO"]
        self.assertEqual(narration, [], "a healthy reconcile journals nothing")

    def stop_daemon(self, daemon):
        """Bound shutdown of the foreground fixture daemon before its tmux server."""
        if daemon.poll() is None:
            daemon.terminate()
        try:
            daemon.wait(timeout=DEADLINE)
        except subprocess.TimeoutExpired:
            daemon.kill()
            daemon.wait(timeout=DEADLINE)

    def test_projects_reuse_healthy_readers_and_recover_closed_windows(self):
        self.reconcile()
        first = self.identity()
        self.reconcile("two")
        control = self.identity("two")
        self.reconcile()
        self.assertEqual(first, self.identity())
        self.tmux("kill-window", "-t", first.split("|")[0])
        self.reconcile()
        self.assertNotEqual(first, self.identity())
        self.assertEqual(control, self.identity("two"))
        self.assertEqual(self.tmux("list-windows", "-a", "-F", "#{window_name}").splitlines(),
                         ["verification", "verification"])

    def test_dead_reader_is_replaced_and_unowned_window_is_preserved(self):
        self.reconcile()
        first = self.identity()
        self.tmux("set-window-option", "-t", first.split("|")[0], "remain-on-exit", "on")
        os.kill(int(first.split("|")[2]), 15)
        end = time.monotonic() + DEADLINE
        while self.tmux("display-message", "-p", "-t", first.split("|")[1], "#{pane_dead}") != "1":
            self.assertLess(time.monotonic(), end)
            time.sleep(.02)
        self.reconcile()
        self.assertNotEqual(first, self.identity())
        self.tmux("new-session", "-d", "-s", "other", "-n", "verification", "sleep", "2147483647")
        other = self.identity("other")
        result = self.reconcile("other", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("ownership", result.stderr)
        self.assertEqual(other, self.identity("other"))


if __name__ == "__main__":
    unittest.main()
