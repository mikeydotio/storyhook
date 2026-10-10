"""Injected stalls only: no host probes, compilers, gates or measurements."""

import errno
import json
import os
from pathlib import Path
import signal
import socket
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import build_products
import gate_measurement_command as command
from host_admission import supervisor
from host_admission.diagnostics import ChildTrace, Trace, boundary
from host_admission.policy import Refusal


class Fixture(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="helper-boundaries-", dir="/tmp")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve()
        self.trace = Trace()
        self.addCleanup(self.trace.close)

    def latest(self, phase, source="child"):
        return self.trace.snapshot()["latest"].get(source + ":" + phase)

    def execute(self, *, spawn=None):
        (self.root / "request.json").write_text(json.dumps({"argv": ["private-argument-sentinel"]}))
        (self.root / "input").write_text("private-input-sentinel")
        with patch.object(command.subprocess, "Popen", spawn or Mock(return_value=SimpleNamespace(
                pid=12345, wait=lambda: 7))):
            return command.execute(self.root, os.dup(self.trace.writer.fileno()))


class CommandBoundaries(Fixture):
    def test_descriptor_stall_precedes_spawn_and_retains_failure(self):
        def stall():
            self.assertEqual(self.latest("descriptor_capture")[2], "begin")
            self.assertIsNone(self.latest("command_spawn"))
            raise OSError("injected descriptor stall")
        spawn = Mock()
        with patch.object(supervisor, "inherited_descriptors", side_effect=stall):
            with self.assertRaisesRegex(OSError, "injected descriptor stall"):
                self.execute(spawn=spawn)
        spawn.assert_not_called()
        self.assertEqual(self.latest("descriptor_capture")[2], "error")
        self.assertFalse((self.root / "result.json").exists())

    def test_spawn_stall_is_not_reported_as_started_command(self):
        def stall(*args, **kwargs):
            self.assertEqual(self.latest("descriptor_capture")[2], "end")
            self.assertEqual(self.latest("command_spawn")[2], "begin")
            self.assertIsNone(self.latest("command_spawn")[4])
            self.assertIsNone(self.latest("command_wait"))
            raise OSError("injected exec handshake stall")
        with self.assertRaisesRegex(OSError, "injected exec handshake stall"):
            self.execute(spawn=Mock(side_effect=stall))
        self.assertEqual(self.latest("command_spawn")[2], "error")

    def test_wait_stall_has_pid_but_no_exit_or_result(self):
        wait = Mock(side_effect=[InterruptedError("injected wait interruption"), 0])
        def waiting():
            if wait.call_count == 0:
                self.assertEqual(self.latest("command_spawn")[4], 321)
                self.assertEqual(self.latest("command_wait")[2], "begin")
            return wait()
        child = SimpleNamespace(pid=321, wait=waiting, kill=Mock())
        with self.assertRaisesRegex(InterruptedError, "injected wait interruption"):
            self.execute(spawn=Mock(return_value=child))
        child.kill.assert_called_once_with()
        self.assertEqual(wait.call_count, 2)
        self.assertEqual(self.latest("command_reap")[2], "end")
        self.assertEqual(self.latest("command_wait")[2], "error")
        self.assertFalse((self.root / "result.json").exists())

    def test_result_fsync_stall_is_distinct_from_command_wait(self):
        def stall(fd):
            self.assertEqual(self.latest("command_wait")[2::2], ["end", 7])
            self.assertEqual(self.latest("result_flush")[2], "end")
            self.assertEqual(self.latest("result_fsync")[2], "begin")
            raise OSError("injected result fsync stall")
        with patch.object(command.os, "fsync", side_effect=stall):
            with self.assertRaisesRegex(OSError, "injected result fsync stall"):
                self.execute()
        self.assertEqual(self.latest("result_fsync")[2], "error")
        # Partial result remains; its existence is never settlement authority.
        self.assertTrue((self.root / "result.json").exists())

    def test_guard_inheritance_streams_and_result_survive_without_telemetry_secrets(self):
        guard = os.open(self.root / "guard", os.O_CREAT | os.O_RDWR, 0o600)
        self.addCleanup(os.close, guard)
        os.set_inheritable(guard, True)
        def spawn(argv, *, stdin, stdout, stderr, pass_fds):
            self.assertIn(guard, pass_fds)
            # Both original and child diagnostic descriptors are non-inheritable.
            self.assertNotIn(self.trace.writer.fileno(), pass_fds)
            self.assertEqual(set(pass_fds), supervisor.inherited_descriptors())
            self.assertEqual(stdin.read(), b"private-input-sentinel")
            stdout.write(b"fixture output"); stderr.write(b"fixture error")
            return SimpleNamespace(pid=12345, wait=lambda: 7)
        self.assertEqual(self.execute(spawn=Mock(side_effect=spawn)), 7)
        result = json.loads((self.root / "result.json").read_text())
        self.assertEqual(result["exit_code"], 7)
        self.assertGreaterEqual(result["wall_seconds"], 0)
        self.assertEqual((self.root / "stdout").read_text(), "fixture output")
        self.assertEqual((self.root / "stderr").read_text(), "fixture error")
        self.assertEqual(self.latest("result_fsync")[2], "end")
        self.assertNotIn("private-", json.dumps(self.trace.snapshot()))


class DiagnosticBounds(Fixture):
    def test_full_child_channel_never_waits_and_reports_later_sequence_gap(self):
        child = ChildTrace(os.dup(self.trace.writer.fileno()))
        self.addCleanup(child.close)
        # Fill the real local nonblocking datagram queue, without a workload.
        for _ in range(10000):
            try:
                self.trace.writer.send(b"[]")
            except OSError as error:
                self.assertIn(error.errno, (errno.EAGAIN, errno.ENOBUFS))
                break
        else:
            self.fail("fixture failed to fill bounded socket queue")
        with patch.object(socket, "setdefaulttimeout", side_effect=AssertionError("must not wait")):
            child.emit("command_spawn", "begin")
        while True:
            try:
                self.trace.reader.recv(256)
            except BlockingIOError:
                break
        child.emit("command_spawn", "end", 42)
        snap = self.trace.snapshot()
        self.assertEqual(snap["child_gaps"], 1)
        self.assertEqual(self.latest("command_spawn")[4], 42)
        self.assertTrue(snap["unobserved_tail_possible"])

    def test_retention_is_bounded_and_preserves_initial_and_latest_boundaries(self):
        self.trace.emit("admission", "begin")
        for _ in range(1000):
            self.trace.emit("census", "begin")
            self.trace.emit("census", "end")
        self.trace.emit("deadline", "error")
        snap = self.trace.snapshot()
        self.assertEqual(len(snap["events"]), 288)
        self.assertEqual(snap["events_evicted"], 1714)
        self.assertEqual(snap["events"][0][1:3], ["admission", "begin"])
        self.assertEqual(snap["latest"]["supervisor:deadline"][2], "error")

    def test_unrecognized_packets_and_secrets_never_enter_trace(self):
        for packet in (b"bad-private-secret", b'[1, [], "begin", 1, null]',
                       b'[1,"command_spawn","begin",1,"private-secret"]'):
            self.trace.writer.send(packet)
        self.trace.emit("private-secret", "begin")
        self.trace.emit("command_spawn", "begin", "private-secret")
        snap = self.trace.snapshot()
        self.assertEqual(snap["invalid_packets"], 3)
        self.assertNotIn("private-secret", json.dumps(snap))

    def test_drain_work_is_bounded_and_disabled_trace_does_not_read_clock(self):
        reader = Mock()
        reader.recv.return_value = b"[]"
        with patch.object(self.trace, "reader", reader):
            self.trace.poll()
        self.assertEqual(reader.recv.call_count, 32)
        with patch("host_admission.diagnostics.time.monotonic_ns", side_effect=AssertionError("clock")):
            self.assertEqual(boundary(None, "census", lambda: 12), 12)

    def test_post_cleanup_save_failure_does_not_replace_failure_or_fsync(self):
        with patch("builtins.open", side_effect=OSError("disk full")), \
                patch.object(os, "fsync", side_effect=AssertionError("diagnostics must not fsync")):
            self.trace.save(self.root / "boundaries.json")
        with patch.object(os, "fsync", side_effect=AssertionError("diagnostics must not fsync")):
            self.trace.save(self.root / "boundaries.json")
        self.assertIn("missing boundaries are unknown", (self.root / "boundaries.json").read_text())

    def test_channel_setup_failure_closes_descriptor_and_command_still_runs(self):
        descriptor = os.dup(self.trace.writer.fileno())
        with patch("host_admission.diagnostics.os.set_inheritable", side_effect=OSError("injected setup")):
            with self.assertRaisesRegex(OSError, "injected setup"):
                ChildTrace(descriptor)
        with self.assertRaises(OSError):
            os.fstat(descriptor)
        with patch("host_admission.diagnostics.os.set_inheritable", side_effect=OSError("injected setup")):
            self.assertEqual(self.execute(), 7)

    def test_channel_close_errors_do_not_replace_results_and_close_both_ends(self):
        child = ChildTrace(os.dup(self.trace.writer.fileno()))
        self.addCleanup(child.close)
        broken = Mock()
        broken.close.side_effect = OSError("injected close")
        with patch.object(child, "socket", broken):
            child.close()
        other = Mock()
        with patch.object(self.trace, "reader", broken), patch.object(self.trace, "writer", other):
            self.trace.close()
        other.close.assert_called_once_with()

    def test_diagnostic_module_ships_with_embedded_supervisor(self):
        # Import-time dependency must travel with extracted helpers, not just Git.
        build = (Path(__file__).resolve().parents[2] / "build.rs").read_text()
        self.assertIn('"host_admission/diagnostics.py",', build)


class CustodyBoundaries(Fixture):
    def custody(self):
        with patch.object(build_products.native, "identity", return_value={"pid": 123}), \
                patch.object(build_products.native, "boot_identity", return_value="fixture"):
            return build_products.ProductCustody(self.root, ["fixture"], trace=self.trace)

    def test_custody_fsync_failure_retains_original_failure_and_boundaries(self):
        def stall(fd):
            self.assertEqual(self.latest("custody_fsync", "supervisor")[2], "begin")
            raise OSError("injected custody fsync failure")
        with patch.object(build_products.os, "fsync", side_effect=stall):
            with self.assertRaisesRegex(OSError, "injected custody fsync failure"):
                self.custody()
        path, = self.root.glob("build-*/boundaries.json")
        snap = json.loads(path.read_text())
        self.assertEqual(snap["latest"]["supervisor:custody_fsync"][2], "error")
        self.assertFalse((path.parent / "record.json").exists())

    def test_directory_fsync_failure_is_not_file_fsync_failure(self):
        calls = []
        def stall(fd):
            calls.append(fd)
            if len(calls) == 2:
                self.assertEqual(self.latest("custody_fsync", "supervisor")[2], "end")
                self.assertEqual(self.latest("custody_directory_fsync", "supervisor")[2], "begin")
                raise OSError("injected directory fsync failure")
        with patch.object(build_products.os, "fsync", side_effect=stall):
            with self.assertRaisesRegex(OSError, "directory fsync failure"):
                self.custody()
        path, = self.root.glob("build-*/record.json")
        self.assertEqual(json.loads(path.read_text())["state"], "reserved")
        self.assertEqual(self.latest("custody_directory_fsync", "supervisor")[2], "error")

    def test_trace_cannot_settle_a_held_guard(self):
        custody = self.custody()
        guard = os.open(custody.root / "guard", os.O_CREAT | os.O_RDWR, 0o600)
        self.addCleanup(os.close, guard)
        build_products.fcntl.flock(guard, build_products.fcntl.LOCK_EX | build_products.fcntl.LOCK_NB)
        custody.call("attach", **custody.lease,
                     execution={"id": "fixture", "guard": "guard", "session": 123})
        self.trace.emit("finish", "end")  # A fabricated diagnostic is no authority.
        with self.assertRaisesRegex(Refusal, "guard is still held"):
            custody.call("settle", **custody.lease, execution_id="fixture")
        row = json.loads((custody.root / "record.json").read_text())
        self.assertEqual(row["state"], "running")
        self.assertEqual(len(row["executions"]), 1)


class SupervisionBoundaries(Fixture):
    def test_real_fixture_launcher_transports_trace_without_leaking_it_to_command(self):
        # Synthetic Python child only. Native host identity/census are fixture
        # seams; this is not a custody proof, host probe or measurement attempt.
        import stat
        expected = [fd for fd in supervisor.inherited_descriptors()
                    if stat.S_ISSOCK(os.fstat(fd).st_mode)]
        code = """
import os, stat
observed = []
for name in os.listdir('/dev/fd'):
    try:
        mode = os.fstat(int(name)).st_mode
    except OSError:
        continue
    if int(name) > 2 and stat.S_ISSOCK(mode):
        observed.append(int(name))
assert sorted(observed) == EXPECTED, 'diagnostic socket leaked to command'
print('fixture child')
""".replace("EXPECTED", repr(sorted(expected)))
        with patch.object(build_products.native, "identity", return_value={"pid": 123}), \
                patch.object(build_products.native, "boot_identity", return_value="fixture"), \
                patch.object(build_products.native, "session_members", return_value=[]):
            result = command.bounded([sys.executable, "-B", "-c", code], root=self.root)
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "fixture child\n")
        path, = self.root.glob("build-*/boundaries.json")
        snap = json.loads(path.read_text())
        for name in ("descriptor_capture", "command_spawn", "command_wait", "result_fsync"):
            self.assertEqual(snap["latest"]["child:" + name][2], "end")
        for name in ("supervisor_descriptor_capture", "supervisor_spawn", "readiness_handshake", "exec_handshake"):
            self.assertEqual(snap["latest"]["supervisor:" + name][2], "end")
        self.assertEqual(json.loads((path.parent / "record.json").read_text())["state"], "finished")

    def process(self, *, publisher=None):
        process = supervisor.ManagedProcess.__new__(supervisor.ManagedProcess)
        process.trace = self.trace
        process.child = SimpleNamespace(pid=12345, wait=Mock(return_value=0))
        process.guard = os.open(self.root / "guard", os.O_CREAT | os.O_RDWR, 0o600)
        self.addCleanup(lambda: os.close(process.guard) if process.guard is not None else None)
        process.finished = False
        process.result = process.failure = process.observation_failure = process.drain_reason = None
        process.forward_signals = False
        process.publisher = publisher
        process.boot = "fixture"
        process.execution_id = "fixture"
        process.timing = {"sample_ms": 100, "cleanup_ms": 5000}
        process.call = Mock(return_value={"state": "running", "executions": [{"id": "fixture"}]})
        return process

    def test_census_and_liveness_stalls_are_separate_with_no_extra_scans(self):
        process = self.process()
        def census(pid):
            self.assertEqual(self.latest("census", "supervisor")[2], "begin")
            self.assertIsNone(self.latest("liveness", "supervisor"))
            return [12, 13]
        def live(pid, session, boot):
            self.assertEqual(self.latest("census", "supervisor")[2], "end")
            self.assertEqual(self.latest("liveness", "supervisor")[2], "begin")
            return True
        with patch.object(supervisor.native, "session_members", side_effect=census) as scan, \
                patch.object(supervisor.native, "session_member_is_live", side_effect=live) as alive:
            self.assertEqual(process._members(), [12, 13])
        scan.assert_called_once_with(12345)
        self.assertEqual(alive.call_count, 2)
        self.assertEqual(self.latest("liveness", "supervisor")[2], "end")

    def test_deadline_failure_drains_term_then_kill_and_never_finishes_record(self):
        publisher = SimpleNamespace(publish=Mock(side_effect=Refusal("deadline expired")))
        process = self.process(publisher=publisher)
        with patch.object(process, "_exited", side_effect=[False, False, True]), \
                patch.object(process, "_members", side_effect=[[12345], [12345], []]), \
                patch.object(process, "_signal") as send, \
                patch.object(supervisor, "time", SimpleNamespace(monotonic_ns=Mock(side_effect=[0, 5_000_000_000]))), \
                patch.object(supervisor.select, "select", return_value=([], [], [])):
            with self.assertRaisesRegex(Refusal, "deadline expired"):
                process.wait()
        self.assertEqual([call.args[1] for call in send.call_args_list], [signal.SIGTERM, signal.SIGKILL])
        self.assertEqual(publisher.publish.call_count, 1)
        self.assertEqual(process.timing, {"sample_ms": 100, "cleanup_ms": 5000})
        self.assertFalse(any(call.args[0] in ("settle", "finish") for call in process.call.call_args_list))
        self.assertIsNone(process.guard)
        self.assertEqual(self.latest("publisher", "supervisor")[2], "error")
        self.assertEqual(self.latest("signal_kill", "supervisor")[2], "end")

    def test_force_cancel_quiet_child_and_surviving_descendant_keep_custody(self):
        process = self.process()
        with patch.object(process, "_exited", return_value=True), \
                patch.object(process, "_members", side_effect=[[222], []]), \
                patch.object(process, "_signal") as send, \
                patch.object(supervisor.select, "select", return_value=([], [], [])):
            self.assertEqual(process.wait(force_cancel=True), 125)
        send.assert_called_once_with([222], signal.SIGTERM)
        self.assertEqual(self.latest("cancellation", "supervisor")[2], "end")
        self.assertEqual([call.args[0] for call in process.call.call_args_list],
                         ["cancel", "inspect", "inspect", "inspect", "settle", "finish"])

    def test_bounded_deadline_is_unchanged_and_retains_first_failure_after_close(self):
        from verifier_state import Refusal as MeasurementRefusal
        from gate_measurement_bounds import Deadline
        now = [0]
        deadlines = []
        def limit(seconds):
            deadlines.append(seconds)
            return Deadline(seconds, clock=lambda: now[0])
        process = SimpleNamespace(close=Mock())
        def managed(custody, lease, argv, **kwargs):
            self.assertFalse(kwargs["grant_environment"])
            self.assertEqual(len(kwargs["pass_fds"]), 1)
            os.fstat(kwargs["pass_fds"][0])
            def wait():
                now[0] = 30
                kwargs["publisher"].publish()
            process.wait = wait
            return process
        with patch.object(build_products.native, "identity", return_value={"pid": 123}), \
                patch.object(build_products.native, "boot_identity", return_value="fixture"), \
                patch("gate_measurement_bounds.Deadline", side_effect=limit), \
                patch.object(supervisor, "ManagedProcess", side_effect=managed):
            with self.assertRaisesRegex(MeasurementRefusal, "exceeds remaining measurement allowance"):
                command.bounded(["fixture"], root=self.root)
        self.assertEqual(deadlines, [30])
        process.close.assert_called_once_with()
        path, = self.root.glob("build-*/boundaries.json")
        snap = json.loads(path.read_text())
        self.assertEqual(snap["latest"]["supervisor:deadline"][2], "error")
        self.assertEqual(snap["latest"]["supervisor:close"][2], "end")
        self.assertEqual(json.loads((path.parent / "record.json").read_text())["state"], "reserved")


if __name__ == "__main__":
    unittest.main()
