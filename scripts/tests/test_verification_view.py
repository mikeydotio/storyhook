"""Exercise the shipping reader reconciler on an owned private tmux server."""

import os
import re
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import load_grace  # noqa: E402

SCRIPT = Path(__file__).resolve().parents[1] / "verification-view.py"
# The daemon runs the reconciler composed after the tmux server policy
# (src/daemon/activity/window.rs); exercise exactly that program.
POLICY = Path(__file__).resolve().parents[2] / "plugins/story/lib/tmux_server_env.py"
PROGRAM = POLICY.read_text() + "\n" + SCRIPT.read_text()
# The reconciler's own bound on one tmux client, read from the shipped script.
RECONCILER_TIMEOUT = int(re.search(r"^TIMEOUT = (\d+)$", SCRIPT.read_text(), re.M).group(1))
DEADLINE = 15  # Includes private server startup and loaded macOS PTY allocation.
# SH-347's ceiling for any one graced wait, as a cap on the multiplier of the
# largest base here (the SH-767 D5 shape).
MAX_GRACE = load_grace.PATIENCE_CEILING / DEADLINE
# One reconciler tick whose only failure was a tmux client outlasting the
# reconciler's per-call bound: what the daemon's next tick retries.
TIMED_OUT = re.compile(r"^verification view: Command '\['tmux', .*\]' timed out after (\d+) seconds$")
# Long enough for several refused readiness probes; only ever spent on a socket
# that is known never to answer.
REFUSAL_PATIENCE = 0.5


def timed_out_client(result):
    """True only when a reconcile's one failure was a tmux client timing out."""
    lines = result.stderr.strip().splitlines()
    match = TIMED_OUT.match(lines[0]) if result.returncode == 1 and len(lines) == 1 else None
    return match is not None and int(match.group(1)) == RECONCILER_TIMEOUT


def diagnosis(what, result):
    """Name a failed child by its role and output, never by its argv.

    The reconcile argv is the whole composed program, which is what a
    CalledProcessError prints instead of the stderr that says why (SH-806).
    """
    return (f"{what} exited {result.returncode}\n"
            f"stderr: {result.stderr.strip() or '(empty)'}\nstdout: {result.stdout.strip() or '(empty)'}")


class ViewTests(unittest.TestCase):
    """Every fixture owns its foreground server and destroys it in cleanup."""

    # The contention reading behind the harness's re-run of a timed-out tick;
    # a regression states its own reading in place of the machine's.
    contention = staticmethod(load_grace.contention)

    def setUp(self):
        # Every bound below is this one graced value (SH-806), sampled once.
        ratio = load_grace.contention()
        self.deadline = load_grace.patience(DEADLINE, ratio)
        if self.deadline > DEADLINE:
            print(f"{self.id()}: {load_grace.describe(ratio, self.deadline / DEADLINE)}", file=sys.stderr)
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
                           timeout=self.deadline, check=True, capture_output=True)
            self.env.update(DYLD_INSERT_LIBRARIES=str(library), STORY_TEST_PTY_FAILURE=str(self.marker))
        server = subprocess.Popen(self.tmux_argv + ["-D"], env=self.env,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.addCleanup(self.stop_server, server)
        self.await_serving(server, self.socket, self.deadline)
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

    def await_serving(self, server, socket, patience):
        """Wait until the fixture server answers a client as itself (SH-806).

        tmux binds its socket before it listens and initialises before it
        serves, so a socket file is not a ready server: the first reconcile
        would pay for server startup inside the reconciler's per-call bound.
        The answer must be this server's own pid, so a server some client
        started in its place is never taken for the one this fixture owns.
        display-message never starts a server, so asking cannot create one.
        """
        argv = [self.tmux_argv[0], "-S", str(socket), "-f", "/dev/null", "display-message", "-p", "#{pid}"]
        end = time.monotonic() + patience
        answer = None
        while True:
            self.assertIsNone(server.poll(), f"fixture server {server.pid} exited before it answered")
            remaining = end - time.monotonic()
            if remaining <= 0:
                self.fail(f"fixture server {server.pid} did not answer at {socket} within {patience:g}s; "
                          + ("no answer" if answer is None else diagnosis("the last probe", answer)))
            try:
                answer = subprocess.run(argv, env=self.env, capture_output=True, text=True, timeout=remaining)
            except subprocess.TimeoutExpired:
                continue
            if answer.returncode == 0 and answer.stdout.strip() == str(server.pid):
                return
            time.sleep(.02)

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
                server.wait(timeout=self.deadline)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait(timeout=self.deadline)
        end = time.monotonic() + self.deadline
        while any(token and self.process_start(pid) == token for pid, token in readers):
            self.assertLess(time.monotonic(), end, "owned pane reader survived private-server cleanup")
            time.sleep(.02)

    def process_start(self, pid):
        """Pair a PID with native start identity to exclude reuse during cleanup."""
        return subprocess.run(["ps", "-p", pid, "-o", "lstart="], capture_output=True,
                              text=True, timeout=self.deadline).stdout.strip()

    def tmux(self, *args, check=True):
        """Bound every private control operation."""
        result = subprocess.run(self.tmux_argv + list(args), env=self.env, capture_output=True,
                                text=True, timeout=self.deadline)
        if check:
            self.assertEqual(result.returncode, 0, diagnosis(f"tmux {args[0]}", result))
        return result.stdout.strip()

    def reconcile(self, project="one", check=True):
        """Run the production helper with literal hostile-path arguments.

        With check, a tick whose only failure was a tmux client outlasting the
        reconciler's own bound runs again while the machine is contended, as
        the daemon's next tick would (SH-806 D3); at idle that is a defect and
        fails at once. Without check, the caller judges exactly one tick.
        """
        directory = self.root / project / "logs with spaces ' $(inert)"
        argv = ["python3", "-c", PROGRAM, project, str(directory), str(self.reader)]
        patience = None
        while True:
            result = subprocess.run(argv, env=self.env, capture_output=True, text=True,
                                    timeout=self.deadline)
            if not check or result.returncode == 0:
                return result
            ratio = self.contention()
            grace = load_grace.multiplier(ratio, MAX_GRACE)
            if not timed_out_client(result) or grace == 1:
                self.fail(diagnosis(f"reconcile of {project}", result))
            now = time.monotonic()
            patience = patience or load_grace.Patience(DEADLINE * grace, grace, MAX_GRACE, now)
            if patience.expired(now):
                self.fail(diagnosis(f"reconcile of {project} still timing out after "
                                    f"{now - patience.started:.1f}s", result))
            print(f"{self.id()}: reconcile of {project} runs again, as the daemon's next tick would; "
                  f"{load_grace.describe(ratio, grace)}: {result.stderr.strip()}", file=sys.stderr)

    def identity(self, project="one"):
        """Return immutable reader identity rather than a reusable index."""
        identity = self.tmux("display-message", "-p", "-t", f"={project}:=verification",
                         "#{window_id}|#{pane_id}|#{pane_pid}")
        self.assertTrue(identity.startswith("@"), "project verification window is absent")
        return identity

    def test_failures_carry_the_failing_programs_own_diagnosis(self):
        # SH-806: a CalledProcessError names the argv (here, the whole composed
        # program) and drops stderr, so a gate red could not say why a reconcile failed.
        wrapper = self.root / "bin/tmux"
        wrapper.write_text(wrapper.read_text().replace(
            "exec ", 'if [ "$1" = list-sessions ]; then echo "fixture diagnosis" >&2; exit 1; fi\nexec ', 1))
        with self.assertRaises(self.failureException) as reconcile:
            self.reconcile()
        self.assertIn("fixture diagnosis", str(reconcile.exception))
        with self.assertRaises(self.failureException) as control:
            self.tmux("list-windows", "-t", "=absent")
        self.assertRegex(str(control.exception), r"tmux list-windows exited 1\nstderr: \S")

    def test_a_bound_socket_is_not_a_serving_server(self):
        # SH-806: tmux binds before it listens and initialises before it serves,
        # so the first reconcile paid for server startup inside its 3 s bound.
        path = self.root / "unlistening.sock"
        holder = subprocess.Popen([sys.executable, "-c", "import socket, sys, time\n"
                                   "s = socket.socket(socket.AF_UNIX)\ns.bind(sys.argv[1])\ntime.sleep(600)",
                                   str(path)])
        self.addCleanup(holder.wait)
        self.addCleanup(holder.kill)
        end = time.monotonic() + self.deadline
        while not path.exists():
            self.assertLess(time.monotonic(), end, "the unlistening socket was never bound")
            time.sleep(.02)
        with self.assertRaises(self.failureException) as refused:
            self.await_serving(holder, path, REFUSAL_PATIENCE)
        self.assertIn("did not answer", str(refused.exception))

    def hold_first(self, verb):
        """Make the first tmux `verb` outlast the reconciler's per-call bound, once."""
        wrapper = self.root / "bin/tmux"
        held = self.root / f"held-{verb}"
        wrapper.write_text(wrapper.read_text().replace(
            "exec ", f'if [ "$1" = {verb} ] && [ ! -e "{held}" ]; then : > "{held}"; '
            f"sleep {RECONCILER_TIMEOUT + 1} </dev/null >/dev/null 2>&1; fi\nexec ", 1))

    def test_a_tick_whose_tmux_client_timed_out_is_run_again_under_contention(self):
        # SH-806 D3: under load the daemon's next tick is the retry.
        self.hold_first("list-sessions")
        self.contention = lambda: 2.0
        self.reconcile()
        self.assertTrue(self.identity().startswith("@"))

    def test_at_idle_a_tmux_client_timeout_still_fails_at_once(self):
        self.hold_first("list-sessions")
        self.contention = lambda: None
        with self.assertRaises(self.failureException) as failed:
            self.reconcile()
        self.assertIn(f"timed out after {RECONCILER_TIMEOUT} seconds", str(failed.exception))

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
            end = time.monotonic() + self.deadline
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
            daemon.wait(timeout=self.deadline)
        except subprocess.TimeoutExpired:
            daemon.kill()
            daemon.wait(timeout=self.deadline)

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
        end = time.monotonic() + self.deadline
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
