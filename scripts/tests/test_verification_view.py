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
BUDGET = POLICY.with_name("probe_budget.py")
sys.path.insert(0, str(POLICY.parent))
from view_program import program
PROGRAM = program(SCRIPT.parent.parent)
# The shipping operation budget also bounds a single client if it is first.
RECONCILER_TIMEOUT = int(re.search(r"^BUDGET_SECONDS = (\d+)$", BUDGET.read_text(), re.M).group(1))
DEADLINE = RECONCILER_TIMEOUT * 3 / 2  # The daemon outer bound, including startup/exit margin.
# SH-347's ceiling for any one graced wait, as a cap on the multiplier of the
# largest base here (the SH-767 D5 shape).
MAX_GRACE = load_grace.PATIENCE_CEILING / DEADLINE
# One reconciler tick whose only failure was a tmux client outlasting the
# reconciler's per-call bound: what the daemon's next tick retries.
TIMED_OUT = re.compile(
    r"^verification view: probe 'tmux .*' did not finish in its [0-9.]+s allowance "
    r"\([0-9.]+s of a (\d+)s operation budget spent; 1-minute load average "
    r"(?:[0-9.]+|unavailable) on (?:\d+|None) cores\)$")
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

    @classmethod
    def setUpClass(cls):
        """Use the production reader's native launch shape, without a second exec."""
        cls.reader_build = tempfile.TemporaryDirectory(prefix='sh825-view-reader-', dir='/tmp')
        cls.addClassCleanup(cls.reader_build.cleanup)
        source = Path(cls.reader_build.name) / 'reader.c'
        cls.reader_binary = source.with_suffix('')
        source.write_text('''#include <stdio.h>
#include <unistd.h>
int main(int argc, char **argv) {
    printf("READER");
    for (int i = 1; i < argc; ++i) printf(" %s", argv[i]);
    printf("\\n");
    fflush(stdout);
    while (1) pause();
}
''')
        subprocess.run(['cc', '-Wall', '-Werror', str(source), '-o', str(cls.reader_binary)],
                       timeout=load_grace.patience(DEADLINE, load_grace.contention()),
                       check=True, capture_output=True)

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
        self.env = dict(os.environ, HOME=str(self.root), STORYHOOK_VERIFIER_MIRROR="1",
                        STORYHOOK_VERIFIER_AGENT="1")
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
        shutil.copy2(self.reader_binary, self.reader)
        # SH-822: never a real provider. A `claude` first on PATH also covers a
        # production daemon that resolves the agent from its own PATH.
        self.agent = bin_dir / "claude"
        self.agent.write_text('#!/bin/sh\nenv > "$HOME/agent-env"\nprintf "AGENT %s\\n" "$*"\n'
                              'exec sleep 2147483647\n')
        self.agent.chmod(0o700)
        self.checkout = self.root / "checkout with spaces"
        self.checkout.mkdir()

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

    def reconcile(self, project="one", check=True, prepared=True, agent=None):
        """Run the production helper with literal hostile-path arguments.

        With check, a tick whose only failure was a tmux client outlasting the
        reconciler's own bound runs again while the machine is contended, as
        the daemon's next tick would (SH-806 D3); at idle that is a defect and
        fails at once. Without check, the caller judges exactly one tick.
        Prepared, the journal directory exists first, as the daemon makes it
        before it runs the view (SH-771).
        """
        directory = self.root / project / "logs with spaces ' $(inert)"
        if prepared:
            directory.mkdir(parents=True, exist_ok=True)
        argv = ["python3", "-c", PROGRAM, project, str(directory), str(self.reader)]
        if agent is not None:
            argv += [str(self.checkout), *agent]
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
        """Return immutable reader identity rather than a reusable index.

        The reader is the pane still running the recorded reader command
        (SH-822: the window can also hold the agent and panes a person
        added), so the identity never depends on which pane is active.
        """
        rows = self.panes(project)
        self.assertTrue(rows, "project verification window is absent")
        reader = [row for row in rows if row["name"] == "verification"
                  and row["command"] and row["start"] == row["command"]] or rows
        return f"{reader[0]['window']}|{reader[0]['pane']}|{reader[0]['pid']}"

    def panes(self, project="one"):
        """Every pane of the project session, including retained user windows."""
        listing = self.tmux("list-panes", "-s", "-t", f"={project}", "-F",
                            "#{window_id}\t#{pane_id}\t#{pane_pid}\t#{pane_left}\t#{pane_dead}"
                            "\t#{pane_current_path}\t#{pane_start_command}\t#{@storyhook-command}\t#{window_name}",
                            check=False)
        keys = ("window", "pane", "pid", "left", "dead", "path", "start", "command", "name")
        # The harness strips its output, so the last row may lose empty fields.
        rows = [dict(zip(keys, line.split("\t") + [""] * len(keys)))
                for line in listing.splitlines() if line.startswith("@")]
        return sorted(rows, key=lambda row: int(row["left"]))

    def agents(self, project="one"):
        """The Verifier Agent panes, known by the marker in their start command."""
        return [row for row in self.panes(project) if "storyhook-verifier:" in row["start"]]

    def wait_for_output(self, pane, text, count=1):
        """Wait, graced, until `pane` has printed `text` at least `count` times."""
        end = time.monotonic() + self.deadline
        while True:
            captured = self.tmux("capture-pane", "-p", "-J", "-t", pane, check=False)
            if captured.count(text) >= count:
                return captured
            self.assertLess(time.monotonic(), end, f"{pane} never printed {text!r}: {captured!r}")
            time.sleep(.05)

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
        self.assertIn(f"{RECONCILER_TIMEOUT}s operation budget spent", str(failed.exception))

    def test_disabled_mirror_does_not_even_probe_tmux(self):
        wrapper = self.root / "bin/tmux"
        wrapper.write_text('#!/bin/sh\nprintf called > "$HOME/called"\nexit 99\n')
        self.env["STORYHOOK_VERIFIER_MIRROR"] = "0"
        self.reconcile(prepared=False)
        self.assertFalse((self.root / "called").exists())
        self.assertFalse((self.root / "one").exists())

    def test_an_unprepared_journal_directory_fails_before_any_tmux_call(self):
        # SH-771: the daemon creates the directory with its ignore file. A
        # view that created it would leave .view.lock where git can see it.
        wrapper = self.root / "bin/tmux"
        wrapper.write_text('#!/bin/sh\nprintf called > "$HOME/called"\nexit 99\n')
        result = self.reconcile(check=False, prepared=False)
        self.assertEqual(result.returncode, 1, diagnosis("reconcile of one", result))
        self.assertIn("absent; the daemon prepares it first", result.stderr)
        self.assertFalse((self.root / "one").exists())
        self.assertFalse((self.root / "called").exists())

    def test_slow_tmux_call_succeeds_in_one_production_pass(self):
        # SH-808: exceed the old 3 s per-client deadline without a harness retry.
        wrapper = self.root / "bin/tmux"
        held = self.root / "slow-call"
        wrapper.write_text(wrapper.read_text().replace(
            "exec ", f'if [ "$1" = list-sessions ] && [ ! -e "{held}" ]; then '
            f': > "{held}"; sleep 4; fi\nexec ', 1))
        result = self.reconcile(check=False)
        self.assertEqual(result.returncode, 0, diagnosis("slow reconcile", result))
        self.assertTrue(self.identity().startswith("@"))

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

    def test_budget_expiry_preserves_old_reader_and_next_pass_reaps_partial_replacement(self):
        from unittest.mock import patch
        from scripts.tests.test_verification_view_budget import program
        self.reconcile()
        first = self.identity()
        self.tmux("set-option", "-w", "-t", first.split("|")[0], "@storyhook-reader", "%999:0")
        view = program()
        clock = [0.0]
        run = subprocess.run

        def answer(argv, **kwargs):
            result = run(argv, **kwargs)
            # Exhaust the real shared clock after allocation and its first identity read.
            if argv[1] == "display-message":
                clock[0] = view.BUDGET_SECONDS
            return result

        directory = self.root / "one" / "logs with spaces ' $(inert)"
        with patch.dict(os.environ, self.env, clear=True), \
                patch.object(view.time, "monotonic", side_effect=lambda: clock[0]), \
                patch.object(view.subprocess, "run", side_effect=answer):
            with self.assertRaises(RuntimeError) as failed:
                view.reconcile("one", str(directory), str(self.reader))
        self.assertIn("operation budget", str(failed.exception))
        self.assertIn("cleanup", str(failed.exception))
        self.assertEqual(first, self.identity())
        self.assertIn("verification-pending-", self.tmux("list-windows", "-t", "=one", "-F", "#{window_name}"))
        self.reconcile()
        self.assertNotEqual(first, self.identity())
        self.assertIn("verification-pending-", self.tmux("list-windows", "-t", "=one", "-F", "#{window_name}"))
        self.reconcile()  # Retirement of the interrupted allocation is a separate pass.
        self.assertEqual(self.tmux("list-windows", "-t", "=one", "-F", "#{window_name}"), "verification")

    def test_allocation_owns_only_the_new_window_before_mark_can_run(self):
        import hashlib
        from unittest.mock import patch
        from scripts.tests.test_verification_view_budget import program
        # Both names could be mistaken for an interrupted allocation. Neither is ours.
        self.tmux("new-session", "-d", "-s", "one", "-n", ".verification-user", str(self.reader), "fixture")
        self.tmux("new-window", "-d", "-t", "=one:", "-n", "verification-pending-user", str(self.reader), "fixture")
        before = self.tmux("list-windows", "-t", "=one", "-F", "#{window_id}|#{window_name}|#{@storyhook-journal}")
        view = program()
        clock = [0.0]
        run = subprocess.run
        allocated = []

        def answer(argv, **kwargs):
            result = run(argv, **kwargs)
            if argv[1] == "new-window":
                allocated.append(result.stdout.strip().split("\t")[0])
                clock[0] = view.BUDGET_SECONDS
            return result

        directory = self.root / "one" / "logs with spaces ' $(inert)"
        # SH-771: the daemon prepares the journal directory before any view.
        directory.mkdir(parents=True, exist_ok=True)
        with patch.dict(os.environ, self.env, clear=True), \
                patch.object(view.time, "monotonic", side_effect=lambda: clock[0]), \
                patch.object(view.subprocess, "run", side_effect=answer):
            with self.assertRaisesRegex(RuntimeError, "operation budget"):
                view.reconcile("one", str(directory), str(self.reader))
        self.assertEqual(len(allocated), 1)
        owner = hashlib.sha256(os.fsencode(directory.resolve())).hexdigest()
        self.assertEqual(self.tmux("show-options", "-wv", "-t", allocated[0], "@storyhook-journal"), owner)
        after = self.tmux("list-windows", "-t", "=one", "-F", "#{window_id}|#{window_name}|#{@storyhook-journal}")
        self.assertEqual("\n".join(after.splitlines()[:2]), before)
        self.reconcile()
        after = self.tmux("list-windows", "-t", "=one", "-F", "#{window_id}|#{window_name}|#{@storyhook-journal}")
        self.assertEqual("\n".join(after.splitlines()[:2]), before)
        self.assertEqual(len(after.splitlines()), 4, "allocation does not also retire an old window")
        self.reconcile()
        after = self.tmux("list-windows", "-t", "=one", "-F", "#{window_id}|#{window_name}|#{@storyhook-journal}")
        self.assertEqual("\n".join(after.splitlines()[:2]), before)
        self.assertEqual(len(after.splitlines()), 3)
        self.assertNotIn(allocated[0] + "|", after)

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
        # Hold one call past 3 s and a later call so the pass exceeds 5 s.
        wrapper = self.root / "bin/tmux"
        prefix = ""
        for verb, delay in (("list-sessions", 4), ("set-option", 2)):
            held = self.root / f"daemon-slow-{verb}"
            prefix += (f'if [ "$1" = {verb} ] && [ ! -e "{held}" ]; then '
                       f': > "{held}"; sleep {delay}; fi\n')
        wrapper.write_text(wrapper.read_text().replace("exec ", prefix + "exec ", 1))
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
        # SH-771: the daemon found this journal directory without an ignore
        # file, as every checkout journaled before SH-771 is, and fixed it.
        self.assertIn("*", (directory / ".gitignore").read_text().splitlines())
        # SH-808: the first real daemon pass exceeded both historical bounds.
        startup_records = [json.loads(line) for line in path.read_text().splitlines() if line.strip()]
        self.assertFalse([row for row in startup_records if row["level"] in ("ERROR", "WARN")
                          and "reader" in row["context"]], startup_records)
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

    @unittest.skipUnless(os.environ.get("STORY_VIEW_TEST_BINARY"), "run via cargo test --test verify_window for production daemon")
    def test_production_daemon_opens_the_verifier_agent_in_its_own_window(self):
        # SH-822: the daemon resolves `claude` on its own PATH and the plugin
        # root that carries agents/verifier.md, and hands the view the launch.
        import json
        binary = os.environ["STORY_VIEW_TEST_BINARY"]
        project = Path(os.environ["STORY_VIEW_TEST_PROJECT"])
        slug = os.environ["STORY_VIEW_TEST_SLUG"]
        directory = project / ".storyhook/logs"
        directory.mkdir(parents=True, exist_ok=True)
        self.assertEqual(shutil.which("claude", path=self.env["PATH"]), str(self.agent),
                         "only the fixture's fake provider may answer to `claude`")
        self.env["STORYHOOK_VERIFIER_AGENT"] = "1"
        # The harness runs a copied binary outside the checkout, and its data
        # directory has no release projection: name the plugin copy, as an
        # operator would for dispatch.
        plugin = Path(__file__).resolve().parents[2] / "plugins/story"
        self.env["STORYHOOK_DISPATCH_SCRIPT"] = str(plugin / "bin/story.sh")
        diagnostics = self.root / "daemon.err"
        with diagnostics.open("wb") as error:
            daemon = subprocess.Popen([binary, "--store-path", os.environ["STORY_VIEW_TEST_STORE"],
                                       "daemon", "--serve", "--port", "0"], cwd=project, env=self.env,
                                      stdout=subprocess.DEVNULL, stderr=error)
        self.addCleanup(self.stop_daemon, daemon)
        end = time.monotonic() + self.deadline
        while not self.agents(slug):
            self.assertIsNone(daemon.poll(), diagnostics.read_text())
            self.assertLess(time.monotonic(), end, f"no agent pane: {self.panes(slug)} "
                            + diagnostics.read_text())
            time.sleep(.05)
        agent = self.agents(slug)[0]
        output = self.wait_for_output(agent["pane"], "--agent story:verifier")
        self.assertIn("--model opus --effort xhigh", output)
        self.assertIn(f"--plugin-dir {plugin}", output)
        self.assertEqual(Path(agent["path"]).resolve(), project.resolve())
        self.assertEqual(agent["name"], "verifier")
        self.assertEqual(len([row for row in self.panes(slug)
                              if row["window"] == agent["window"]]), 1)
        self.stop_daemon(daemon)
        journal = [json.loads(line) for path in directory.glob("*.jsonl")
                   for line in path.read_text().splitlines() if line.strip()]
        self.assertFalse([row for row in journal if "Verifier Agent" in row.get("message", "")
                          and row["level"] == "WARN"], journal)

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
        import json
        self.reconcile()
        first = self.identity()
        proof = json.loads(self.tmux('show-options', '-p', '-v', '-t', first.split('|')[1],
                                     '@storyhook-reader-proof-v1'))
        self.assertEqual(proof['process']['process']['executable'], str(self.reader.resolve()))
        self.assertEqual(proof['process']['argv'][0], str(self.reader.resolve()))
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

    # --- SH-861: reader and agent occupy separate owned windows -------------

    def launch(self):
        """The agent argv as the daemon composes it, with the fake provider."""
        return [str(self.agent), "--agent", "story:verifier", "--model", "opus", "--effort", "xhigh"]

    def with_agent(self):
        """Create the window, then (the next pass) its agent; return both panes."""
        self.reconcile(agent=self.launch())
        self.reconcile(agent=self.launch())
        agents = self.agents()
        self.assertEqual(len(agents), 1, self.panes())
        self.wait_for_output(agents[0]["pane"], "AGENT --agent story:verifier")
        return agents[0], self.identity()

    def test_without_agent_arguments_the_view_stays_single_pane(self):
        self.reconcile()
        self.reconcile()
        self.assertEqual(len(self.panes()), 1)
        self.assertEqual(self.agents(), [])

    def test_fresh_view_uses_two_detached_single_pane_windows(self):
        self.tmux("new-session", "-d", "-s", "one", "-n", "personal", "sleep", "2147483647")
        focused = self.tmux("display-message", "-p", "-t", "=one", "#{window_id}:#{pane_id}")
        self.reconcile(agent=self.launch())
        self.assertEqual(len(self.panes()), 2, "one structural change per pass: the reader first")
        reader = self.identity()
        self.reconcile(agent=self.launch())
        agent, after = self.with_agent()
        self.assertEqual(reader, after, "adding the agent never replaces the reader")
        panes = self.panes()
        owned = [row for row in panes if row["name"] in ("verification", "verifier")]
        self.assertEqual({row["name"] for row in owned}, {"verification", "verifier"})
        self.assertEqual(len(owned), 2)
        self.assertNotEqual(agent["window"], reader.split("|")[0])
        self.assertEqual(focused, self.tmux("display-message", "-p", "-t", "=one", "#{window_id}:#{pane_id}"))
        owners = [self.tmux("show-option", "-w", "-v", "-t", row["window"],
                            "@storyhook-journal") for row in owned]
        self.assertTrue(owners[0])
        self.assertEqual(owners[0], owners[1])
        self.assertEqual(Path(agent["path"]).resolve(), self.checkout.resolve())
        output = self.wait_for_output(agent["pane"], "AGENT")
        self.assertIn("AGENT --agent story:verifier --model opus --effort xhigh", output)
        self.reconcile(agent=self.launch())
        self.assertEqual(self.panes(), panes, "a healthy window is left alone")

    def test_the_agent_pane_carries_the_pane_overrides(self):
        self.env.update(GH_TOKEN="parent-secret", STORYHOOK_STORE_PATH="/fixture/store.db")
        self.with_agent()
        dump = self.root / "agent-env"
        end = time.monotonic() + self.deadline
        while not dump.exists():
            self.assertLess(time.monotonic(), end, "the agent never started")
            time.sleep(.05)
        seen = dict(line.split("=", 1) for line in dump.read_text().splitlines() if "=" in line)
        self.assertEqual(seen.get("GH_TOKEN"), "", "a parent's GitHub credential never reaches the agent")
        self.assertEqual(seen.get("STORYHOOK_STORE_PATH"), "/fixture/store.db")

    def test_agent_and_user_panes_are_healthy_not_a_conflict(self):
        agent, reader = self.with_agent()
        self.tmux("split-window", "-d", "-t", reader.split("|")[1], "sleep", "2147483647")
        before = self.panes()
        self.assertEqual(len(before), 3)
        self.reconcile(agent=self.launch())
        self.assertEqual(self.panes(), before)

    def test_two_owned_verification_windows_are_an_ownership_conflict(self):
        self.reconcile()
        first = self.identity()
        owner = self.tmux("show-option", "-w", "-v", "-t", first.split("|")[0], "@storyhook-journal")
        second = self.tmux("new-window", "-d", "-P", "-F", "#{window_id}", "-t", "=one:",
                           "-n", "verification", "sleep", "2147483647")
        self.tmux("set-option", "-w", "-t", second, "@storyhook-journal", owner)
        result = self.reconcile(check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("ownership conflict", result.stderr)

    def test_dead_reader_window_is_replaced_without_touching_the_agent(self):
        agent, reader = self.with_agent()
        window, pane, pid = reader.split("|")
        self.tmux("set-window-option", "-t", window, "remain-on-exit", "on")
        os.kill(int(pid), 15)
        end = time.monotonic() + self.deadline
        while self.tmux("display-message", "-p", "-t", pane, "#{pane_dead}") != "1":
            self.assertLess(time.monotonic(), end)
            time.sleep(.02)
        self.reconcile(agent=self.launch())
        replaced = self.identity()
        self.assertNotEqual(reader, replaced)
        self.assertNotEqual(replaced.split("|")[0], window, "only the reader window is replaced")
        self.assertEqual(self.agents(), [agent])
        self.assertEqual({row["pane"] for row in self.panes()},
                         {agent["pane"], replaced.split("|")[1]})

    def test_closed_reader_returns_in_a_separate_window(self):
        agent, reader = self.with_agent()
        self.tmux("kill-pane", "-t", reader.split("|")[1])
        self.reconcile(agent=self.launch())
        returned = self.identity()
        self.assertNotEqual(reader, returned)
        self.assertEqual(self.agents(), [agent])
        self.assertEqual(len(self.panes()), 2)

    def test_closed_agent_window_respects_persisted_cooldown(self):
        agent, reader = self.with_agent()
        self.tmux("kill-window", "-t", agent["window"])
        self.reconcile(agent=self.launch())
        self.assertEqual(self.agents(), [], "a just-created agent is not restarted within the cooldown")
        window = reader.split("|")[0]
        owner = self.tmux("show-option", "-w", "-v", "-t", window, "@storyhook-journal")
        session = self.tmux("display-message", "-p", "-t", "=one", "#{session_id}")
        self.tmux("set-option", "-t", session, "@storyhook-agent-started-" + owner,
                  str(int(time.time()) - 61))
        self.reconcile(agent=self.launch())
        self.assertEqual(len(self.agents()), 1)
        self.assertEqual(reader, self.identity())

    def test_duplicate_legacy_agents_are_a_conflict_and_never_killed(self):
        import hashlib
        self.reconcile(agent=self.launch())
        reader = self.identity()
        directory = (self.root / "one" / "logs with spaces ' $(inert)").resolve()
        owner = hashlib.sha256(os.fsencode(directory)).hexdigest()
        # Two passes that both split before either saw the other's pane.
        for _ in range(2):
            self.tmux("split-window", "-d", "-h", "-b", "-t", reader.split("|")[1],
                      "/bin/sh", "-c", 'exec "$@"', "storyhook-verifier:" + owner, str(self.agent))
        agents = self.agents()
        self.assertEqual(len(agents), 2)
        result = self.reconcile(agent=self.launch(), check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("ownership conflict", result.stderr)
        self.assertEqual(self.agents(), agents, "no live duplicate is killed")

    def test_the_agent_waits_for_enter_after_it_exits(self):
        self.agent.write_text('#!/bin/sh\nprintf "AGENT %s\\n" "$*"\nexit 3\n')
        self.reconcile(agent=self.launch())
        self.reconcile(agent=self.launch())
        agents = self.agents()
        self.assertEqual(len(agents), 1)
        pane = agents[0]["pane"]
        self.wait_for_output(pane, "exited (status 3). Press Enter to start it again.")
        self.reconcile(agent=self.launch())
        self.assertEqual([row["pane"] for row in self.agents()], [pane], "the pane stays; no restart loop")
        self.tmux("send-keys", "-t", pane, "Enter")
        self.wait_for_output(pane, "AGENT --agent story:verifier", count=2)

    def legacy_agent(self):
        """An SH-822 reader + fake live agent, created only on our private server."""
        self.reconcile()
        reader = self.identity()
        owner = self.tmux("show-option", "-w", "-v", "-t", reader.split("|")[0], "@storyhook-journal")
        self.tmux("set-option", "-w", "-t", reader.split("|")[0], "@storyhook-agent-started",
                  str(int(time.time())))
        self.tmux("split-window", "-d", "-h", "-t", reader.split("|")[1],
                  "/bin/sh", "-c", 'while :; do "$@"; read -r _ || exit; done',
                  "storyhook-verifier:" + owner, *self.launch())
        agent = self.agents()[0]
        self.wait_for_output(agent["pane"], "AGENT")
        return reader, agent

    def test_legacy_split_migration_preserves_live_agent_pid(self):
        reader, agent = self.legacy_agent()
        start = self.process_start(agent["pid"])
        self.assertTrue(start)
        self.reconcile(agent=self.launch())
        migrated = self.agents()[0]
        self.assertEqual((migrated["pane"], migrated["pid"]), (agent["pane"], agent["pid"]))
        self.assertEqual(self.process_start(agent["pid"]), start)
        self.assertEqual(migrated["name"], "verifier")
        self.assertNotEqual(migrated["window"], agent["window"])
        self.assertEqual(self.identity(), reader)
        self.assertEqual(len(self.panes()), 2)
        self.reconcile(agent=self.launch())
        self.assertEqual(self.agents(), [migrated])

    def test_dead_reader_preserves_user_pane_in_released_window(self):
        self.reconcile()
        reader = self.identity()
        window, pane, pid = reader.split("|")
        user = self.tmux("split-window", "-d", "-t", pane, "-P", "-F", "#{pane_id}:#{pane_pid}",
                         "sleep", "2147483647")
        self.tmux("set-window-option", "-t", window, "remain-on-exit", "on")
        os.kill(int(pid), 15)
        end = time.monotonic() + self.deadline
        while self.tmux("display-message", "-p", "-t", pane, "#{pane_dead}") != "1":
            self.assertLess(time.monotonic(), end)
            time.sleep(.02)
        self.reconcile()
        retained = self.tmux("display-message", "-p", "-t", window, "#{window_name}")
        self.assertTrue(retained.startswith("verification-retained-"))
        self.assertEqual(self.tmux("show-option", "-w", "-q", "-v", "-t", window, "@storyhook-journal"), "")
        self.assertEqual(len(self.panes()), 2, "release is the only structural change this pass")
        self.reconcile()
        self.assertNotEqual(self.identity().split("|")[0], window)
        self.assertEqual(self.tmux("display-message", "-p", "-t", user.split(":")[0],
                                  "#{pane_id}:#{pane_pid}"), user)
        self.assertEqual(self.tmux("display-message", "-p", "-t", pane, "#{pane_dead}"), "1")

    def test_disabled_agent_preserves_legacy_agent_without_creation(self):
        reader, agent = self.legacy_agent()
        self.env["STORYHOOK_VERIFIER_AGENT"] = "0"
        self.reconcile(agent=self.launch())
        self.assertEqual(self.identity(), reader)
        self.assertEqual(self.agents(), [agent])
        self.assertEqual({row["name"] for row in self.panes()}, {"verification"})
        self.reconcile("two", agent=self.launch())
        self.reconcile("two", agent=self.launch())
        self.assertEqual(len(self.panes("two")), 1)
        self.assertEqual(self.agents("two"), [])

    def test_verifier_name_conflicts_preserve_foreign_and_duplicate_windows(self):
        self.reconcile()
        foreign = self.tmux("new-window", "-d", "-t", "=one:", "-n", "verifier", "-P",
                            "-F", "#{window_id}", "sleep", "2147483647")
        before = self.panes()
        result = self.reconcile(agent=self.launch(), check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("ownership conflict", result.stderr)
        self.assertEqual(before, self.panes())
        self.tmux("rename-window", "-t", foreign, "personal")
        self.reconcile(agent=self.launch())
        agent = self.agents()[0]
        owner = self.tmux("show-option", "-w", "-v", "-t", agent["window"], "@storyhook-journal")
        duplicate = self.tmux("new-window", "-d", "-t", "=one:", "-n", "verifier", "-P",
                              "-F", "#{window_id}", "sleep", "2147483647")
        self.tmux("set-option", "-w", "-t", duplicate, "@storyhook-journal", owner)
        before = self.panes()
        result = self.reconcile(agent=self.launch(), check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("ownership conflict", result.stderr)
        self.assertEqual(before, self.panes())

    def test_legacy_agent_without_reader_renames_without_restarting(self):
        reader, agent = self.legacy_agent()
        self.tmux("kill-pane", "-t", reader.split("|")[1])
        self.reconcile(agent=self.launch())
        moved = self.agents()[0]
        self.assertEqual((moved["pane"], moved["pid"], moved["window"]),
                         (agent["pane"], agent["pid"], agent["window"]))
        self.assertEqual(moved["name"], "verifier")
        self.assertEqual(len(self.panes()), 1)
        self.reconcile(agent=self.launch())
        self.assertEqual(len(self.panes()), 2)
        self.assertNotEqual(self.identity().split("|")[0], agent["window"])

    def test_disabled_legacy_agent_survives_reader_release_and_later_migration(self):
        reader, agent = self.legacy_agent()
        window, pane, pid = reader.split("|")
        self.tmux("set-window-option", "-t", window, "remain-on-exit", "on")
        os.kill(int(pid), 15)
        end = time.monotonic() + self.deadline
        while self.tmux("display-message", "-p", "-t", pane, "#{pane_dead}") != "1":
            self.assertLess(time.monotonic(), end)
            time.sleep(.02)
        self.env["STORYHOOK_VERIFIER_AGENT"] = "0"
        self.reconcile(agent=self.launch())
        self.reconcile(agent=self.launch())
        self.assertEqual(len(self.agents()), 1)
        self.assertNotEqual(self.identity().split("|")[0], window)
        self.assertFalse([row for row in self.panes() if row["name"] == "verifier"])
        self.env["STORYHOOK_VERIFIER_AGENT"] = "1"
        self.reconcile(agent=self.launch())
        moved = self.agents()[0]
        self.assertEqual((moved["pane"], moved["pid"]), (agent["pane"], agent["pid"]))
        self.assertEqual(moved["name"], "verifier")
        self.assertEqual(self.tmux("show-option", "-w", "-q", "-v", "-t", window,
                                  "@storyhook-journal"), "")


    def test_cleanup_retires_one_pending_window_before_agent_allocation(self):
        self.reconcile()
        window = self.identity().split("|")[0]
        owner = self.tmux("show-option", "-w", "-v", "-t", window, "@storyhook-journal")
        for suffix in ("first", "second"):
            pending = self.tmux("new-window", "-d", "-t", "=one:", "-n", "verification-pending-" + suffix,
                                "-P", "-F", "#{window_id}", "sleep", "2147483647")
            self.tmux("set-option", "-w", "-t", pending, "@storyhook-journal", owner)
        for expected in (2, 1):
            self.reconcile(agent=self.launch())
            self.assertEqual(len(self.panes()), expected)
            self.assertEqual(self.agents(), [])
        self.reconcile(agent=self.launch())
        self.assertEqual(len(self.agents()), 1)


class ReleasedViewTests(unittest.TestCase):
    def test_restored_released_reader_requires_exact_release_witness(self):
        # Exercise the restoration boundary without a server or process probe.
        from types import SimpleNamespace
        scope = {"__name__": "verification_view_test"}
        exec(PROGRAM, scope)
        owner = "fixture-owner"
        row = ["@4", "verification-retained-fixture", "%7", "123", "1", "", "", "old", "", ""]
        scope["restore_evidence"] = lambda *_: {"panes": {
            "reader-uuid": {"pane_id": "%7", "window": {"options": {"@storyhook-journal": owner}}}
        }}
        scope["probe_run"] = lambda *_args, **_kwargs: SimpleNamespace(returncode=0, stdout="", stderr="")
        commands = []
        witness = owner

        def tmux(*args):
            commands.append(args)
            if args[0] == "list-panes":
                return "%7\treader-uuid"
            self.assertEqual(args, ("show-option", "-w", "-q", "-v", "-t", "@4", "@storyhook-view-released"))
            return witness

        scope["tmux"] = tmux
        self.assertFalse(scope["readopt_view"]([row], owner))
        self.assertEqual(len(commands), 2)
        witness = "different-owner"
        with self.assertRaisesRegex(RuntimeError, "moved, duplicated or foreign"):
            scope["readopt_view"]([row], owner)


class NoSplitProductionTests(unittest.TestCase):
    def test_production_sources_never_split_tmux_windows(self):
        root = SCRIPT.parent.parent
        offenders = []
        for directory in ("src", "scripts", "plugins/story"):
            for path in (root / directory).rglob("*"):
                if (not path.is_file() or "tests" in path.relative_to(root).parts
                        or path.name.startswith(("test_", "test-")) or "__pycache__" in path.parts):
                    continue
                if b"split-window" in path.read_bytes():
                    offenders.append(str(path.relative_to(root)))
        self.assertEqual(offenders, [], "production tmux creation must allocate whole windows")


if __name__ == "__main__":
    unittest.main()
