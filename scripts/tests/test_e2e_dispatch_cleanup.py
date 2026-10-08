"""SH-807: exercise the actual wrapper and cleanup without browsers or a daemon."""

import importlib.util
import json
import os
from pathlib import Path
import shlex
import signal
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
