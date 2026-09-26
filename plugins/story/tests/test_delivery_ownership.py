"""Fence notification authority with real locks and exact process captures."""

import fcntl
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch


PLUGIN = Path(os.environ.get("STORY_TEST_PLUGIN_ROOT", Path(__file__).resolve().parents[1]))
sys.path.insert(0, str(PLUGIN / "lib"))


def module(name, filename):
    """Load a production adapter without starting a provider or daemon."""
    spec = importlib.util.spec_from_file_location(name, PLUGIN / "lib" / filename)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


IDENTITY = module("authority_identity", "agent_identity.py")
INTERRUPT = module("authority_interrupt", "interrupt-agent.py")


class InterruptOwnershipTests(unittest.TestCase):
    """Ancestry is fixture data; production capture and comparison are exercised."""

    def test_frozen_descendants_retain_native_incarnation(self):
        """The shared comparator needs a native token as well as ps ancestry."""
        table = {401: (1, "same second"), 402: (401, "same second")}
        owned = {}
        with patch.object(INTERRUPT, "processes", return_value=table), \
                patch.object(INTERRUPT.proc, "process_identity", side_effect=lambda pid: {"start": f"native:{pid}"}), \
                patch.object(INTERRUPT.os, "kill") as killed:
            INTERRUPT.freeze(owned, {401: (1, "same second", "native:401")})
            self.assertEqual(owned[402], (401, "same second", "native:402"))
            self.assertTrue(INTERRUPT.proc.alive(402, owned[402]))
            with patch.object(INTERRUPT.proc, "process_identity", return_value={"start": "replacement"}):
                self.assertFalse(INTERRUPT.proc.alive(402, owned[402]))
            killed.assert_any_call(402, signal.SIGSTOP)

    def test_target_uses_native_start_not_second_precision_ps(self):
        """A recycled PID within one ps timestamp must create another target."""
        def probe(*args):
            return "401" if args[-1] == "#{pane_pid}" else "codex" if args[-1] == "@storyhook-agent" else "/private/socket"
        with patch.object(INTERRUPT, "processes", return_value={401: (1, "same second")}), \
                patch.object(INTERRUPT.proc, "run", side_effect=probe), \
                patch.object(INTERRUPT.proc, "process_identity", side_effect=[{"start": "native:1"}, {"start": "native:2"}]):
            first = json.loads(INTERRUPT.target("%1", "codex"))
            second = json.loads(INTERRUPT.target("%1", "codex"))
        self.assertEqual(first[3], "native:1")
        self.assertNotEqual(first, second)


class AdoptionTests(unittest.TestCase):
    """Discovery must not silently turn a current pane into delayed authority."""

    def test_resolve_returns_unregistered_observation_without_writing(self):
        """Only the locked ordinary notification path may adopt the observation."""
        pane = {"pane": "%1", "window": "TST-1", "cwd": "/fixture/worktree", "dead": False, "socket": "/fixture/socket"}
        record = {"provider": "codex", "pane": "%1"}
        with patch.dict(os.environ, {"STORYHOOK_NOTIFY_LEASE_V1": "null"}), \
                patch.object(IDENTITY, "panes", return_value={"%1": pane}), \
                patch.object(IDENTITY, "context", return_value={"common": "/fixture/common"}), \
                patch.object(IDENTITY, "read_record", return_value=None), \
                patch.object(IDENTITY, "eligible_worktree", return_value=True), \
                patch.object(IDENTITY, "direct_provider", return_value="codex"), \
                patch.object(IDENTITY, "tmux", return_value=""), \
                patch.object(IDENTITY, "observe", return_value=record), \
                patch.object(IDENTITY, "write_record", side_effect=AssertionError("discovery wrote registration")):
            self.assertEqual(IDENTITY.resolve("project", "TST-1", "TST-1"), (record, True))

    def test_adoption_requires_the_exact_held_story_workspace(self):
        """An inherited lock for another story cannot authorize registration."""
        record = {"common": "/unused", "story": "TST-1"}
        with patch.dict(os.environ, {}, clear=True):
            with self.assertRaisesRegex(ValueError, "workspace ownership"):
                IDENTITY.adopt(record)
        with tempfile.TemporaryDirectory(prefix="story-adoption-lock-", dir="/tmp") as tmp:
            common = Path(tmp)
            locks = common / "storyhook/workspace-locks"
            locks.mkdir(parents=True)
            (locks / "TST-1.lock").touch()
            record["common"] = str(common)
            with (locks / "TST-2.lock").open("a+") as wrong:
                with patch.dict(os.environ, {"STORY_WORKSPACE_LOCK_FD": str(wrong.fileno())}):
                    with self.assertRaisesRegex(ValueError, "workspace identity changed"):
                        IDENTITY.adopt(record)

    def test_adoption_revalidates_the_observed_provider_before_write(self):
        """A provider replacement after revocation gets no registration."""
        record = dict(common="/fixture", story="TST-1", project="project", pane="%1",
                      socket="/socket", worktree="/worktree", provider="codex")
        pane = {"window": "TST-1", "dead": False}
        with patch.object(IDENTITY, "require_workspace"), \
                patch.object(IDENTITY, "pane_at", return_value=pane), \
                patch.object(IDENTITY, "read_record", return_value=None), \
                patch.object(IDENTITY, "context", return_value={}), \
                patch.object(IDENTITY, "direct_provider", return_value="claude"), \
                patch.object(IDENTITY, "write_record") as write:
            with self.assertRaisesRegex(IDENTITY.IdentityError, "provider changed"):
                IDENTITY.adopt(record)
            write.assert_not_called()


class RevocationReceiptTests(unittest.TestCase):
    """Drive the actual shell receipt gate with malformed endpoint responses."""

    def test_only_one_successful_identity_bound_receipt_permits_exposure(self):
        """Reject wrong identities, invalid counts, protocol drift and CLI errors."""
        valid = dict(protocol_version=1, project="project", story_id="TST-1", superseded=0)
        cases = [(json.dumps(valid), 0, True), (json.dumps({**valid, "superseded": 2}), 0, True)]
        cases += [(json.dumps({**valid, key: value}), 0, False) for key, value in (
            ("protocol_version", 2), ("project", "another"), ("story_id", "TST-2"),
            ("superseded", -1), ("superseded", .5), ("superseded", "1"),
            ("superseded", True), ("superseded", None))]
        cases += [(json.dumps(valid), 7, False), ("not JSON", 0, False),
                  (json.dumps(valid) + "\n" + json.dumps(valid), 0, False), ("[]", 0, False)]
        with tempfile.TemporaryDirectory(prefix="story-revocation-receipt-", dir="/tmp") as tmp:
            root = Path(tmp)
            for index, (receipt, status, accepted) in enumerate(cases):
                with self.subTest(receipt=receipt, status=status):
                    marker = root / str(index)
                    with (root / "workspace.lock").open("a+") as owner:
                        fcntl.flock(owner, fcntl.LOCK_EX | fcntl.LOCK_NB)
                        env = dict(os.environ, RECEIPT=receipt, ENDPOINT_STATUS=str(status),
                                   STORY_WORKSPACE_LOCK_FD=str(owner.fileno()), PROJECT_SLUG="project")
                        answer = subprocess.run(["bash", "-c", '''
source "$1"
exec 9<&$STORY_WORKSPACE_LOCK_FD
export STORY_WORKSPACE_LOCK_FD=9
story_cli() {
  [ "$*" = "internal supersede-block-deliveries TST-1 --json" ] || return 91
  python3 - "$3" <<'PYFD'
import fcntl,os
os.fstat(9)
fcntl.flock(9, fcntl.LOCK_EX | fcntl.LOCK_NB)
PYFD
  [ "$?" = 0 ] || return 92
  printf '%s' "$RECEIPT"
  return "$ENDPOINT_STATUS"
}
supersede_block_deliveries TST-1 || exit 17
printf exposed > "$2"
''', "fixture", str(PLUGIN / "lib/workspace.sh"), str(marker)], env=env,
                            pass_fds=(owner.fileno(),), capture_output=True, text=True, timeout=5)
                        self.assertEqual(answer.returncode, 0 if accepted else 17, answer.stderr)
                        self.assertEqual(marker.exists(), accepted)


class ChildOwnershipTests(unittest.TestCase):
    """The effect child must retain exclusion when its controller disappears."""

    def test_notification_probe_child_keeps_workspace_lock_after_parent_exit(self):
        """Exercise the actual imported subprocess runner with a bounded child."""
        self.assert_child_ownership("stop-dispatch-pane.py", "p.run(sys.executable,*sys.argv[2:])")

    def test_identity_effect_child_keeps_workspace_lock_after_parent_exit(self):
        """Registration clients must not outlive their authority when orphaned."""
        self.assert_child_ownership("agent_identity.py", "p.run(sys.executable,*sys.argv[2:])")

    def test_continuation_child_keeps_workspace_lock_after_parent_exit(self):
        """Nested continuation handoff preserves any admission inherited from Rust."""
        self.assert_child_ownership("continuation_runtime.py", "p.command([sys.executable,*sys.argv[2:]])")

    def assert_child_ownership(self, filename, invocation):
        """Orphan a real effect client at a filesystem barrier, then release it."""
        with tempfile.TemporaryDirectory(prefix="story-delivery-fd-", dir="/tmp") as tmp:
            root = Path(tmp)
            lock_path = root / "workspace.lock"
            ready = root / "ready"
            release = root / "release"
            done = root / "done"
            child = root / "effect.py"
            child.write_text("import os,pathlib,sys,time\n"
                             "pathlib.Path(sys.argv[1]).write_text(str(os.getpid()))\n"
                             "deadline=time.monotonic()+10\n"
                             "while not pathlib.Path(sys.argv[2]).exists() and time.monotonic()<deadline: time.sleep(.01)\n"
                             "pathlib.Path(sys.argv[3]).touch()\n")
            with lock_path.open("a+") as owner:
                fcntl.flock(owner, fcntl.LOCK_EX | fcntl.LOCK_NB)
                env = dict(os.environ, STORY_WORKSPACE_LOCK_FD=str(owner.fileno()))
                controller = subprocess.Popen([sys.executable, "-c",
                    "import importlib.util,sys; sys.path.insert(0,sys.argv[1]); "
                    f"s=importlib.util.spec_from_file_location('probe',sys.argv[1]+'/{filename}'); "
                    "p=importlib.util.module_from_spec(s); s.loader.exec_module(p); " + invocation,
                    str(PLUGIN / "lib"), str(child), str(ready), str(release), str(done)],
                    env=env, pass_fds=(owner.fileno(),), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                try:
                    deadline = time.monotonic() + 5
                    while not ready.exists() and time.monotonic() < deadline:
                        time.sleep(.01)
                    self.assertTrue(ready.exists(), "effect child did not reach the barrier")
                    controller.kill()
                    controller.wait(timeout=5)
                    owner.close()
                    with lock_path.open("a+") as competitor:
                        with self.assertRaises(BlockingIOError):
                            fcntl.flock(competitor, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    release.touch()
                    deadline = time.monotonic() + 5
                    while True:
                        with lock_path.open("a+") as competitor:
                            try:
                                fcntl.flock(competitor, fcntl.LOCK_EX | fcntl.LOCK_NB)
                                break
                            except BlockingIOError:
                                self.assertLess(time.monotonic(), deadline, "effect child retained ownership after release")
                        time.sleep(.01)
                finally:
                    release.touch()
                    if controller.poll() is None:
                        controller.kill()
                    controller.wait(timeout=5)
                    if ready.exists():
                        deadline = time.monotonic() + 5
                        while not done.exists() and time.monotonic() < deadline:
                            time.sleep(.01)
                        self.assertTrue(done.exists(), "effect child did not finish after release")


if __name__ == "__main__":
    unittest.main()
