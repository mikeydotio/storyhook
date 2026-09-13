#!/usr/bin/env python3
"""SH-702: completed execution remains visible when cleanup cannot finish."""

import json
import os
import tempfile
from unittest import mock
from pathlib import Path
import shutil
import sys
import unittest

from test_verifier_lifecycle import VerifierLifecycle

# A fixture command can consume the production cleanup budget plus scheduling
# overhead; all fixtures own their processes and never touch the real tmux server.
CLEANUP_MS = 4000
PATIENCE = CLEANUP_MS / 1000 + 30


class VerdictPreservation(unittest.TestCase):
    """Drive the real script chain with OS failures in a private script bundle."""

    def setUp(self):
        """Reuse the real-Git fixture without inheriting its unrelated test cases."""
        self.fx = VerifierLifecycle()
        self.fx.setUp()
        self.addCleanup(self.fx.doCleanups)
        self.bundle = self.fx.root / "bundle"
        shutil.copytree(Path(__file__).resolve().parents[1], self.bundle)
        self.fx.env["STORYHOOK_VERIFIER_CLEANUP_GRACE_MS"] = str(CLEANUP_MS)
        self.fx.env["TMUX_TMPDIR"] = str(self.fx.root / "tmux")
        Path(self.fx.env["TMUX_TMPDIR"]).mkdir()
        self.assertEqual(self.fx.ensure()["result"], "verifier-worktree-ready")

    def fault_census(self, field):
        """Make the OS census unavailable only after the selected leader exited."""
        script = self.bundle / "verifier-owner.py"
        source = script.read_text()
        injection = '''
_real_members = session_members
def session_members(sid):
    import pathlib
    for path in pathlib.Path(sys.argv[2]).glob('storyhook/verifier-lifecycle/*.owner'):
        record = read(path)
        if record.get('gate_leader_exit') is not None and FIELD == 'gate_session':
            raise OSError('SH-702 injected post-gate census failure')
        if FIELD == 'session' and record.get('session') == sid:
            # This call happens only after the lifecycle leader has answered.
            raise OSError('SH-702 injected outer census failure')
    return _real_members(sid)
'''.replace('FIELD', repr(field))
        source = source.replace('if __name__ == "__main__":', injection + '\nif __name__ == "__main__":')
        script.write_text(source)

    def gate(self, status, script=None, command=None):
        """Run the production gate seam and require exactly one JSON document."""
        tree = self.fx.git("merge-tree", "--write-tree", self.fx.base, self.fx.head)
        gate_command = command or ["bash", "-c", script or
                                   f"printf 'test sh702_named_failure ... FAILED\\n'; exit {status}"]
        result = self.fx.command("bash", str(self.bundle / "verify-pr.sh"), "--run-gate", "1", tree,
                                 self.fx.base, self.fx.head, str(self.fx.wt), "--", *gate_command)
        return json.loads(result.stdout)

    def test_inner_refusal_preserves_red_and_its_named_failure(self):
        """A gate exit must survive the inner owner's post-exit census error."""
        self.fault_census("gate_session")
        result = self.gate(3)
        self.assertEqual(result["result"], "tests-failed", result)
        self.assertIn("sh702_named_failure", result["detail"])
        self.assertEqual(result["exit_status"], 3)
        self.assertIn("cleanup_failure", result)
        owner = json.loads(next((self.fx.common / "storyhook/verifier-lifecycle").glob("*.owner")).read_text())
        self.assertTrue(owner["gate_started"])

    def test_outer_refusal_preserves_red_json(self):
        """The outer owner must not discard a complete buffered RED result."""
        self.fault_census("session")
        result = self.gate(7)
        self.assertEqual(result["result"], "tests-failed", result)
        self.assertIn("sh702_named_failure", result["detail"])
        self.assertIn("outer census", result["cleanup_failure"]["detail"])

    def test_passed_gate_with_cleanup_failure_is_not_a_merge(self):
        """Gate execution success must remain distinct from permission to land."""
        self.fault_census("gate_session")
        result = self.gate(0)
        self.assertEqual(result["result"], "gate-passed", result)
        self.assertEqual(result["exit_status"], 0)
        self.assertIn("cleanup_failure", result)

    def inject(self, name, code):
        """Install an OS fault only in this fixture's private script copy."""
        script = self.bundle / name
        source = script.read_text()
        script.write_text(source.replace('if __name__ == "__main__":', code + '\nif __name__ == "__main__":'))

    def test_final_owner_save_preserves_red(self):
        """A failed atomic replacement of the final owner is also cleanup."""
        self.inject("verifier-owner.py", '''
_replace = os.replace
def replace(source, target):
    if str(target).endswith('.owner') and json.loads(open(source).read()).get('completed'):
        raise OSError('SH-702 final owner save refused')
    return _replace(source, target)
os.replace = replace
''')
        result = self.gate(3)
        self.assertEqual(result["result"], "tests-failed", result)
        self.assertIn("final owner save", result["cleanup_failure"]["detail"])

    def test_restoration_failure_preserves_red(self):
        """Restoring the git pointer is independent of the gate's observed exit."""
        self.inject("verifier-worktree.py", '''
_replace = os.replace
def replace(source, target):
    if sys.argv[1] == 'recover' and str(target).endswith('/.git'):
        raise OSError('SH-702 restoration replace refused')
    return _replace(source, target)
os.replace = replace
''')
        result = self.gate(3)
        self.assertEqual(result["result"], "tests-failed", result)
        self.assertIn("restoration replace", result["cleanup_failure"]["detail"])

    def test_signal_during_restoration_preserves_red(self):
        """A signalled wrapper must not reclassify an already completed command."""
        self.inject("verifier-worktree.py", '''
if sys.argv[1] == 'recover':
    import signal
    os.kill(os.getppid(), signal.SIGTERM)
''')
        result = self.gate(3)
        self.assertEqual(result["result"], "tests-failed", result)
        self.assertIn("cleanup_failure", result)

    def test_verifier_signal_trap_preserves_completed_red(self):
        """The shell's own signal trap sees the same completed evidence."""
        script = self.bundle / "verify-pr.sh"
        source = script.read_text()
        script.write_text(source.replace('verification_phase="release gate"',
                                         'verification_phase="release gate"\n    export SH702_PID=$$'))
        self.inject("verifier-worktree.py", '''
if sys.argv[1] == 'recover':
    import signal
    os.kill(int(os.environ['SH702_PID']), signal.SIGTERM)
''')
        result = self.gate(3)
        self.assertEqual(result["result"], "tests-failed", result)
        self.assertIn("cleanup_failure", result)

    def test_outer_refusal_preserves_an_already_landed_merge(self):
        """The outer result recovery applies to guarded merges as well as RED."""
        self.fault_census("session")
        answer = {"result": "merged", "tree": "judged", "detail": "guarded merge landed"}
        result = self.fx.command("python3", str(self.bundle / "verifier-owner.py"), "run-json",
                                 str(self.fx.common), str(self.fx.wt), "--", "python3", "-c",
                                 "print(" + repr(json.dumps(answer)) + ")")
        result = json.loads(result.stdout)
        self.assertEqual(result["result"], "merged", result)
        self.assertIn("cleanup_failure", result)

    def test_launch_failure_is_not_a_test_failure(self):
        """The supervisor's 125 is distinguishable from a gate that exits 125."""
        result = self.gate(125, command=[str(self.fx.root / "missing-command")])
        self.assertEqual(result["result"], "infrastructure-failure", result)
        result = self.gate(125)
        self.assertEqual(result["result"], "tests-failed", result)
        self.assertEqual(result["exit_status"], 125)

    def test_terminated_gate_is_unjudged(self):
        """SH-692 still excludes actual command signal termination."""
        result = self.gate(143, script="kill -TERM $$")
        self.assertEqual(result["result"], "infrastructure-failure", result)
        self.assertEqual(result["disposition"], "retryable", result)

    def test_gate_cannot_inherit_evidence_paths(self):
        """Only the supervisor can publish through the private evidence channel."""
        result = self.gate(0, script='test -z "${STORYHOOK_GATE_EXECUTION_FILE+x}${STORYHOOK_GATE_RESULT_FILE+x}"')
        self.assertEqual(result["result"], "gate-passed", result)
        self.assertNotIn("cleanup_failure", result)

    def test_retained_orphan_refuses_readmission_without_losing_red(self):
        """A real survivor remains evidence when the OS refuses signalling it."""
        pid_path = self.fx.root / "orphan"
        self.inject("verifier-owner.py", '''
_kill = os.kill
def kill(pid, sig):
    from pathlib import Path
    marker = Path(PID_PATH)
    if marker.exists() and marker.read_text().strip() == str(pid):
        raise PermissionError('SH-702 orphan signal refused')
    return _kill(pid, sig)
os.kill = kill
'''.replace('PID_PATH', repr(str(pid_path))))
        import shlex
        result = self.gate(3, script=f"sleep 300 & echo $! > {shlex.quote(str(pid_path))}; echo 'test orphan_failure ... FAILED'; exit 3")
        pid = int(pid_path.read_text())
        self.addCleanup(lambda: self.fx.stop_pid(pid))
        self.assertEqual(result["result"], "tests-failed", result)
        self.assertIn("orphan_failure", result["detail"])
        self.assertIn("orphan signal refused", result["cleanup_failure"]["detail"])
        admission = self.fx.ensure()
        self.assertEqual(admission["result"], "infrastructure-failure", admission)
        # A descendant can retain the inherited flock before admission even
        # reaches its census; both proofs correctly retain the same owner.
        self.assertIn("owner", admission["detail"])

    def test_unlink_failure_preserves_red(self):
        """Removing the restoration marker is cleanup, not execution."""
        import shlex
        fake = self.fx.root / "interpreters/rm"
        fake.write_text('#!/bin/bash\nfor value in "$@"; do\ncase "$value" in\n*pr-1-result.*) echo "SH-702 unlink refused" >&2; exit 1;;\nesac\ndone\nexec ' + shlex.quote(shutil.which("rm")) + ' "$@"\n')
        fake.chmod(0o755)
        result = self.gate(3)
        self.assertEqual(result["result"], "tests-failed", result)
        self.assertIn("could not remove gate completion", result["cleanup_failure"]["detail"])

    def test_outer_refusal_does_not_accept_partial_json(self):
        """A child that never published one valid answer cannot invent a verdict."""
        self.fault_census("session")
        result = self.fx.command("python3", str(self.bundle / "verifier-owner.py"), "run-json",
                                 str(self.fx.common), str(self.fx.wt), "--", "python3", "-c",
                                 "print('{\"result\":\"tests-failed\"')")
        value = json.loads(result.stdout)
        self.assertEqual(value["result"], "infrastructure-failure", value)
        self.assertIn("outer census", value["detail"])


class ExecutionEvidence(unittest.TestCase):
    """Malformed or foreign execution records never manufacture a verdict."""

    def test_only_normal_matching_execution_can_be_read(self):
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        import verifier_result as result
        from verifier_state import Refusal, save
        with tempfile.TemporaryDirectory(dir="/tmp") as root, mock.patch.dict(os.environ, {"STORYHOOK_VERIFIER_OWNER": "owner-one"}):
            path = Path(root) / "attempt.json"
            argv = ["verifier_result.py", "init", str(path), "tree", "base", "head"]
            with mock.patch.object(sys, "argv", argv):
                result.main()
            original = json.loads(path.read_text())
            for status, launched, valid in [(0, True, True), (3, True, True), (125, True, True), (125, False, False), (143, True, False)]:
                save(path, original)
                result.publish_execution(path, status, launched)
                with mock.patch.object(sys, "argv", [argv[0], "read", *argv[2:]]), mock.patch("builtins.print") as printed:
                    if valid:
                        result.main()
                        printed.assert_called_once_with(status)
                    else:
                        with self.assertRaises(Refusal): result.main()
            for field, value in [("owner", "another-owner"), ("attempt", "different.json"), ("tree", "other-tree"), ("exit_status", True), ("state", "pending")]:
                altered = dict(original, state="completed", exit_status=3)
                altered[field] = value
                save(path, altered)
                with mock.patch.object(sys, "argv", [argv[0], "read", *argv[2:]]):
                    with self.assertRaises(Refusal): result.main()
            path.write_text("{broken")
            with self.assertRaises(Refusal): result.execution(path)
            path.unlink()
            with self.assertRaises(Refusal): result.execution(path)


if __name__ == "__main__":
    unittest.main(defaultTest=["VerdictPreservation", "ExecutionEvidence"])
