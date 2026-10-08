"""SH-807: exercise the actual wrapper and cleanup without browsers or a daemon."""

import contextlib
import importlib.util
import json
import os
from pathlib import Path
import shlex
import signal
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

from load_grace import contention, patience

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
SPEC = importlib.util.spec_from_file_location("dispatch_owners", SCRIPTS / "e2e-dispatch-owners.py")
owners = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(owners)
RUNNER = (SCRIPTS / "run-e2e.sh").read_text()


class DispatchCleanupTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="story-SH807-", dir="/tmp")
        self.outer = Path(self.scratch.name)
        self.root = self.outer / "slice"
        self.root.mkdir()
        self.registry = self.root / "dispatch-owners"
        owners.initialize(self.registry)
        self.state = self.root / "faketmux"
        self.state.mkdir()
        self.knobs = self.root / "knobs"
        self.knobs.mkdir()
        (self.knobs / "FAKE_TMUX_STATE").write_text(str(self.state))
        self.bin = self.outer / "bin"
        self.bin.mkdir()
        (self.bin / "python3").symlink_to(sys.executable)
        self.executable("tmux", '#!/bin/bash\ntouch "$FAKE_TMUX_STATE/accessed"\n')
        self.env = dict(os.environ, PATH=str(self.bin) + os.pathsep + os.environ["PATH"])
        self.budget = patience(8, contention())
        # These waits deliberately exhaust custody refusal, not await progress.
        # Preserve their half-second idle allowance while granting host-load grace.
        self.refusal_budget = patience(0.5, contention())
        self.processes = []
        self.identities = []

    def tearDown(self):
        # Only processes this test created, with their captured native identity.
        for value in self.identities:
            if owners.same(value):
                os.kill(value["pid"], signal.SIGKILL)
        for proc, identity in self.processes:
            if owners.same(identity):
                os.killpg(proc.pid, signal.SIGKILL)
            proc.communicate(timeout=self.budget)
        self.scratch.cleanup()

    def executable(self, name, body):
        path = self.bin / name
        path.write_text(body)
        path.chmod(0o700)
        return path

    def wait_for(self, predicate, diagnostic):
        deadline = time.monotonic() + self.budget
        while not predicate():
            if time.monotonic() >= deadline:
                self.fail(diagnostic)
            time.sleep(0.01)

    def spawn(self, args):
        proc = subprocess.Popen(args, env=self.env, start_new_session=True,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        identity = owners.observed(proc.pid)
        self.assertIsNotNone(identity, "fixture process exited before identity capture")
        self.processes.append((proc, identity))
        return proc

    def render_wrapper(self, body):
        helper = self.outer / f"real-helper-{len(self.processes)}.sh"
        helper.write_text(body)
        wrapper = self.root / f"dispatch-wrapper-{len(self.processes)}.sh"
        block = RUNNER.split('  cat >"$STORYHOOK_DISPATCH_SCRIPT" <<WRAPPER\n', 1)[1]
        block = 'cat >"$STORYHOOK_DISPATCH_SCRIPT" <<WRAPPER\n' + block.split('\nWRAPPER', 1)[0] + '\nWRAPPER\n'
        env = dict(self.env, STORYHOOK_DISPATCH_SCRIPT=str(wrapper),
                   faketmux_env=str(self.knobs), dispatch_owners=str(self.registry),
                   dispatch_owner_tool=str(SCRIPTS / "e2e-dispatch-owners.py"),
                   _real_dispatch_script=str(helper), _dispatch_protocol="DISPATCH_PROTOCOL=fixture")
        result = subprocess.run(["/bin/bash", "-c", block], env=env, capture_output=True,
                                text=True, timeout=self.budget)
        self.assertEqual(result.returncode, 0, result.stderr)
        return wrapper

    def writer(self, *, completes=False, verb="dispatch"):
        marker = self.outer / f"writer-{verb}.json"
        writer = self.outer / f"writer-{verb}.py"
        writer.write_text(
            "import json, os, pathlib, signal, sys, time\n"
            f"sys.path.insert(0, {str(SCRIPTS)!r})\n"
            "from host_admission import native\n"
            "signal.signal(signal.SIGTERM, signal.SIG_IGN)\n"
            f"pathlib.Path({str(marker)!r}).write_text(json.dumps(native.process(os.getpid(), 'fixture')))\n"
            f"root = pathlib.Path({str(self.state)!r})\n"
            "while True:\n"
            " root.mkdir(parents=True, exist_ok=True)\n"
            " (root / 'input').write_text('late write')\n"
            " time.sleep(0.01)\n"
        )
        body = f'{shlex.quote(sys.executable)} {shlex.quote(str(writer))} </dev/null >/dev/null 2>&1 &\n'
        body += f'while [ ! -s {shlex.quote(str(marker))} ]; do sleep 0.01; done\n'
        body += "exit 7\n" if completes else "wait\n"
        wrapper = self.render_wrapper(body)
        proc = self.spawn(["/bin/bash", str(wrapper), "--project", "fixture", verb])
        self.wait_for(marker.exists, "detached helper did not publish its writer")
        # Atomicity is irrelevant to production here; the readiness file is test-owned.
        def read_identity():
            try:
                return json.loads(marker.read_text())
            except (ValueError, FileNotFoundError):
                return None
        self.wait_for(read_identity, "writer identity was not published completely")
        identity = read_identity()
        self.identities.append(identity)
        return proc, identity

    def cleanup_driver(self, writer):
        # The fake stop verifies that the real cleanup drained first. No installed
        # story command, production store or tmux server is used by this fixture.
        stopped = self.outer / "daemon-stopped"
        story = self.executable("story", f'''#!{sys.executable}
import pathlib, sys
sys.path.insert(0, {str(SCRIPTS)!r})
from host_admission import native
try:
 value = native.process({writer['pid']}, 'fixture')
 live = value['live'] and value['start'] == {writer['start']!r}
except ProcessLookupError:
 live = False
if live:
 sys.exit(21)
pathlib.Path({str(stopped)!r}).write_text('stopped after drain')
''')
        body = RUNNER.split("  cleanup() {", 1)[1].split("\n  }\n", 1)[0]
        ready = self.outer / "cleanup-ready"
        driver = self.outer / "slice.sh"
        driver.write_text(f'''#!/bin/bash
set -uo pipefail
data_root={shlex.quote(str(self.root))}
dispatch_owners={shlex.quote(str(self.registry))}
dispatch_owner_tool={shlex.quote(str(SCRIPTS / 'e2e-dispatch-owners.py'))}
dispatch_owners_ready=1
E2E_STOP_GRACE_SECONDS=10
isolated=1
story_bin={shlex.quote(str(story))}
slice_started=$SECONDS
e2e_timing() {{ :; }}
cleanup() {{{body}
}}
trap cleanup EXIT
trap 'exit 143' TERM
trap 'exit 130' INT
trap 'exit 129' HUP
touch {shlex.quote(str(ready))}
while :; do sleep 0.01; done
''')
        proc = self.spawn(["/bin/bash", str(driver)])
        self.wait_for(ready.exists, "cleanup owner did not install its traps")
        return proc, stopped

    def interrupted(self, sig, status):
        helper, writer = self.writer()
        unrelated = self.spawn(["/bin/sleep", "300"])
        slice_proc, stopped = self.cleanup_driver(writer)
        self.assertNotEqual(os.getpgid(helper.pid), os.getpgid(slice_proc.pid))
        slice_proc.send_signal(sig)
        output, errors = slice_proc.communicate(timeout=self.budget)
        self.assertEqual(slice_proc.returncode, status, output + errors)
        self.assertTrue(stopped.exists(), "daemon stop must follow writer drain")
        self.assertFalse(owners.same(writer), errors)
        self.assertIsNone(unrelated.poll(), "cleanup must preserve an unrelated group")
        self.assertFalse(self.root.exists(), errors)
        self.wait_for(lambda: helper.poll() is not None, "helper survived removal")
        self.assertFalse(self.root.exists(), "writer recreated the removed root")

    def test_term_drains_detached_writer_before_removal(self):
        self.interrupted(signal.SIGTERM, 143)

    def test_int_drains_detached_writer_before_removal(self):
        self.interrupted(signal.SIGINT, 130)

    def test_hup_drains_detached_writer_before_removal(self):
        self.interrupted(signal.SIGHUP, 129)

    def test_successful_helper_records_placeholder_and_preserves_status(self):
        helper, writer = self.writer(completes=True)
        output, errors = helper.communicate(timeout=self.budget)
        self.assertEqual(helper.returncode, 7, output + errors)
        receipt = json.loads((self.registry / f"{helper.pid}.json").read_text())
        self.assertIn(writer["pid"], [row["pid"] for row in receipt["completed"]])
        owners.close(self.registry, time.monotonic() + self.budget)
        owners.drain(self.registry, time.monotonic() + self.budget)
        self.assertFalse(owners.same(writer))

    def test_overlapping_unclaim_is_registered_and_drained_too(self):
        dispatch, first = self.writer()
        unclaim, second = self.writer(verb="unclaim")
        self.assertIsNone(dispatch.poll())
        self.assertIsNone(unclaim.poll())
        self.assertTrue((self.registry / f"{unclaim.pid}.json").exists())
        owners.close(self.registry, time.monotonic() + self.budget)
        owners.drain(self.registry, time.monotonic() + self.budget)
        self.assertFalse(owners.same(first))
        self.assertFalse(owners.same(second))

    def test_late_unclaim_refuses_before_first_fake_tmux_access(self):
        wrapper = self.render_wrapper("exit 0\n")
        owners.close(self.registry, time.monotonic() + self.budget)
        helper = self.spawn(["/bin/bash", str(wrapper), "--project", "fixture", "unclaim"])
        output, errors = helper.communicate(timeout=self.budget)
        self.assertEqual(helper.returncode, 70, output + errors)
        self.assertIn("closed helper admission", errors)
        self.assertFalse((self.state / "accessed").exists())

    def test_reused_identity_is_preserved_and_cleanup_keeps_evidence(self):
        helper, writer = self.writer()
        receipt_path = self.registry / f"{helper.pid}.json"
        receipt = json.loads(receipt_path.read_text())
        receipt["owner"]["start"] += "-stale"
        receipt_path.write_text(json.dumps(receipt))
        slice_proc, _ = self.cleanup_driver(writer)
        slice_proc.send_signal(signal.SIGTERM)
        _, errors = slice_proc.communicate(timeout=self.budget)
        self.assertEqual(slice_proc.returncode, 143, errors)
        self.assertIn("incarnation changed", errors)
        self.assertIn("retained", errors)
        self.assertTrue(self.root.exists())
        self.assertIsNone(helper.poll())
        self.assertTrue(owners.same(writer))

    def escaped_writer(self):
        """A real registered helper forks a writer that changes its session."""
        marker = self.outer / "escaped.json"
        heartbeat = self.outer / "heartbeat"
        child = self.outer / "escaped.py"
        child.write_text(
            "import json, os, pathlib, sys, time\n"
            f"sys.path.insert(0, {str(SCRIPTS)!r})\n"
            "from host_admission import native\n"
            f"pathlib.Path({str(marker)!r}).write_text(json.dumps(native.process(os.getpid(), 'fixture')))\n"
            "count = 0\n"
            "while True:\n"
            f" root = pathlib.Path({str(self.state)!r})\n"
            " root.mkdir(parents=True, exist_ok=True)\n"
            " (root / 'escaped-input').write_text('owned writer')\n"
            " count += 1\n"
            f" pathlib.Path({str(heartbeat)!r}).write_text(str(count))\n"
            " time.sleep(0.01)\n"
        )
        launcher = self.outer / "escape-launcher.py"
        launcher.write_text(
            "import pathlib, subprocess, sys, time\n"
            f"subprocess.Popen([sys.executable, {str(child)!r}], start_new_session=True, "
            "pass_fds=(9,), stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)\n"
            f"while not pathlib.Path({str(heartbeat)!r}).exists(): time.sleep(0.01)\n"
        )
        wrapper = self.render_wrapper(
            f'exec 9<{shlex.quote(str(self.registry / "writers"))}\n'
            f'{shlex.quote(sys.executable)} -B {shlex.quote(str(SCRIPTS / "e2e-dispatch-owners.py"))} '
            f'writer-admit {shlex.quote(str(self.registry))} 9 || exit 64\n'
            f'{shlex.quote(sys.executable)} {shlex.quote(str(launcher))}\nexit 7\n'
        )
        helper = self.spawn(["/bin/bash", str(wrapper), "--project", "fixture", "dispatch"])
        _, errors = helper.communicate(timeout=self.budget)
        self.assertEqual(helper.returncode, 7, errors)
        identity = json.loads(marker.read_text())
        self.identities.append(identity)
        self.assertNotEqual(os.getpgid(identity["pid"]), helper.pid)
        self.assertEqual(os.getsid(identity["pid"]), identity["pid"])
        return identity, heartbeat

    def test_escaped_session_writer_preserves_root_until_custody_released(self):
        writer, _ = self.escaped_writer()
        with self.assertRaisesRegex(owners.UnsafeCleanup, "writer custody remains"):
            owners.cleanup(self.registry, self.root, "unused", False, time.monotonic() + self.refusal_budget)
        self.assertTrue(self.root.exists())
        self.assertTrue(owners.same(writer), "unknown session must not be signalled")
        self.assertTrue((self.state / "escaped-input").exists())
        os.kill(writer["pid"], signal.SIGKILL)
        self.wait_for(lambda: not owners.same(writer), "test writer did not exit")
        owners.cleanup(self.registry, self.root, "unused", False, time.monotonic() + self.budget)
        self.assertFalse(self.root.exists())

    def test_outer_kill_does_not_leave_escaped_writer_frozen(self):
        writer, heartbeat = self.escaped_writer()
        cleaner = self.spawn([sys.executable, "-B", str(SCRIPTS / "e2e-dispatch-owners.py"),
                              "cleanup", str(self.registry), str(self.root), "unused", "0", "8"])
        self.wait_for(lambda: (self.registry / "lock").read_text() == "closed",
                      "cleanup did not close admission")
        cleaner.kill()
        cleaner.communicate(timeout=self.budget)
        self.assertEqual(cleaner.returncode, -signal.SIGKILL)
        self.assertTrue(self.root.exists())
        def count():
            try:
                return int(heartbeat.read_text())
            except ValueError:
                return -1
        before = count()
        self.wait_for(lambda: count() > before + 2, "escaped writer was stranded frozen")
        self.assertTrue(owners.same(writer))

    def test_cleanup_uses_one_deadline_for_all_phases(self):
        clock = [100.0]
        deadlines = []
        def phase(root, deadline):
            deadlines.append(deadline)
            clock[0] += 2
        def stop(*args, **kwargs):
            self.assertEqual(kwargs["timeout"], 4.0)
            clock[0] += 4
            return subprocess.CompletedProcess(args[0], 0, stderr=b"")
        with mock.patch.object(owners.time, "monotonic", side_effect=lambda: clock[0]), \
             mock.patch.object(owners, "close", side_effect=phase), \
             mock.patch.object(owners, "drain", side_effect=phase), \
             mock.patch.object(owners, "exclusive_writers", return_value=contextlib.nullcontext()), \
             mock.patch.object(owners.subprocess, "run", side_effect=stop) as stopped, \
             mock.patch.object(owners.shutil, "rmtree") as removed:
            with self.assertRaisesRegex(owners.UnsafeCleanup, "aggregate.*deadline"):
                owners.cleanup(self.registry, self.root, "fixture-story", True, 108.0)
        self.assertEqual(deadlines, [108.0, 108.0])
        stopped.assert_called_once()
        removed.assert_not_called()
        self.assertTrue(self.root.exists())

    def test_closed_real_fake_and_baked_doubles_refuse_before_mutation(self):
        fake = SCRIPTS.parent / "plugins/story/tests/fakes/tmux"
        provider_bin = self.outer / "providers"
        generation = ('source "$1"; write_e2e_provider_doubles "$2" "$3" "$4" "$5" "$6"')
        result = subprocess.run(["/bin/bash", "-c", generation, "fixture",
                                 str(SCRIPTS / "e2e-provider-doubles.sh"), str(provider_bin),
                                 str(self.knobs), str(fake), str(self.registry),
                                 str(SCRIPTS / "e2e-dispatch-owners.py")], env=self.env,
                                capture_output=True, text=True, timeout=self.budget)
        self.assertEqual(result.returncode, 0, result.stderr)
        owners.close(self.registry, time.monotonic() + self.budget)
        shutil.rmtree(self.knobs)
        env = dict(self.env, FAKE_TMUX_STATE=str(self.state), FAKE_TMUX_CUSTODY=str(self.registry),
                   FAKE_TMUX_CUSTODY_HELPER=str(SCRIPTS / "e2e-dispatch-owners.py"),
                   FAKE_TMUX_SESSIONS="must-not-seed")
        for executable, args in [(fake, ["new-window"]), (provider_bin / "tmux", ["new-window"]),
                                 (provider_bin / "codex", ["exec", "fixture"])]:
            with self.subTest(executable=str(executable)):
                invocation_env = dict(env)
                if executable != fake:
                    # Only baked custody can protect these late calls: neither
                    # the snapshot nor ambient custody variables are available.
                    invocation_env.pop("FAKE_TMUX_CUSTODY")
                    invocation_env.pop("FAKE_TMUX_CUSTODY_HELPER")
                result = subprocess.run([str(executable), *args], env=invocation_env, capture_output=True,
                                        text=True, timeout=self.budget)
                self.assertEqual(result.returncode, 64, result.stderr)
                self.assertIn("closed writer admission", result.stderr)
                self.assertEqual(list(self.state.iterdir()), [])

    def test_real_delayed_publisher_retains_custody_after_foreground_exit(self):
        # Use the real fake's admission and background scheduling. Replace only
        # the external provider hook with a barrier: no production CLI or hook.
        source = (SCRIPTS.parent / "plugins/story/tests/fakes/tmux").read_text()
        marker = self.outer / "publisher.json"
        release = self.outer / "release-publisher"
        published = self.outer / "published"
        publisher = self.outer / "publisher.py"
        publisher.write_text(
            "import json, os, pathlib, sys, time\n"
            f"sys.path.insert(0, {str(SCRIPTS)!r})\n"
            "from host_admission import native\n"
            f"pathlib.Path({str(marker)!r}).write_text(json.dumps(native.process(os.getpid(), 'fixture')))\n"
            f"while not pathlib.Path({str(release)!r}).exists(): time.sleep(0.01)\n"
            f"pathlib.Path({str(self.state / 'delayed-write')!r}).write_text('published')\n"
            f"pathlib.Path({str(published)!r}).write_text('done')\n"
        )
        before, function = source.split("publish_claude_hook() {", 1)
        _, after = function.split("\n}\n", 1)
        fake = self.executable("delayed-tmux", before + "publish_claude_hook() {\n" +
                               f"  {shlex.quote(sys.executable)} {shlex.quote(str(publisher))}\n" +
                               "}\n" + after)
        self.env.update(FAKE_TMUX_STATE=str(self.state), FAKE_TMUX_CUSTODY=str(self.registry),
                        FAKE_TMUX_CUSTODY_HELPER=str(SCRIPTS / "e2e-dispatch-owners.py"),
                        FAKE_TMUX_SENTINEL_DELAY_SECS="0.01", FAKE_TMUX_PANE_LIFETIME="300")
        foreground = self.spawn([str(fake), "new-window", "-c", str(self.outer), "claude", ";",
                                 "set-window-option", "remain-on-exit", "on"])
        _, errors = foreground.communicate(timeout=self.budget)
        self.assertEqual(foreground.returncode, 0, errors)
        self.wait_for(marker.exists, "delayed publisher did not reach its barrier")
        def identity_ready():
            try:
                return json.loads(marker.read_text())
            except ValueError:
                return None
        self.wait_for(identity_ready, "publisher identity was incomplete")
        self.identities.append(identity_ready())
        pane = owners.observed(int((self.state / "pane_pid").read_text()))
        self.assertIsNotNone(pane)
        self.identities.append(pane)
        os.kill(pane["pid"], signal.SIGKILL)
        self.wait_for(lambda: not owners.same(pane), "placeholder did not exit")
        try:
            with self.assertRaisesRegex(owners.UnsafeCleanup, "writer custody remains"):
                owners.cleanup(self.registry, self.root, "unused", False, time.monotonic() + self.refusal_budget)
            self.assertTrue(self.root.exists())
        finally:
            release.touch()
        self.wait_for(published.exists, "delayed publisher did not finish")
        owners.cleanup(self.registry, self.root, "unused", False, time.monotonic() + self.budget)
        self.assertFalse(self.root.exists())

    def test_open_descriptor_retains_closed_admission_after_unlink(self):
        owners.close(self.registry, time.monotonic() + self.budget)
        # A late waiter opened the inode before rmtree unlinked its name.
        stream = (self.registry / "lock").open("r+")
        (self.registry / "lock").unlink()
        with mock.patch.object(Path, "open", return_value=stream):
            with self.assertRaisesRegex(owners.UnsafeCleanup, "closed helper admission"):
                owners.register(self.registry, os.getpid(), time.monotonic() + self.budget)
        self.assertEqual(list(self.registry.glob("*.json")), [])


if __name__ == "__main__":
    unittest.main()
