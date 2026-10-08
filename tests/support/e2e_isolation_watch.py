"""Scheduled isolation contracts against real disposable Git repositories."""
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import plistlib
import selectors
import signal
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("watcher", ROOT / "scripts/e2e-isolation-watch.py")
watcher = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(watcher)
GRACE_SPEC = importlib.util.spec_from_file_location("load_grace", ROOT / "scripts/tests/load_grace.py")
grace = importlib.util.module_from_spec(GRACE_SPEC)
GRACE_SPEC.loader.exec_module(grace)


class WatchTests(unittest.TestCase):
    """External proof/tool availability are fixtures; Git, locks and watcher are real."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="isolation-watch-", dir="/tmp")
        self.addCleanup(self.tmp.cleanup)
        self.base = Path(self.tmp.name)
        self.controller = self.base / "controller"
        self.controller.mkdir()
        self.git("init", "-q", "-b", "dev")
        self.git("config", "user.email", "test@example.com")
        self.git("config", "user.name", "Test")
        for name, text in {
            ".gitignore": "e2e/node_modules/\n",
            "scripts/branch-policy.sh": "STORYHOOK_INTEGRATION_BRANCH=dev\n",
            "e2e/package-lock.json": "{}\n",
            "scripts/run-e2e.sh": '''#!/bin/bash
set -eu
[ "$1" = --isolate-files ]
[ "${CI:-}" = 1 ]
[ "${STORYHOOK_E2E_JOBS:-}" = 8 ]
mkdir -p "$STORYHOOK_E2E_RESULTS_DIR"
printf '%s\\n' '{"schema":1,"selected_tests":1,"selected_files":1,"failures":[],"exit_code":0}' > "$STORYHOOK_E2E_RESULTS_DIR/isolation.json"
echo proof-executed
''',
        }.items():
            file = self.controller / name
            file.parent.mkdir(parents=True, exist_ok=True)
            file.write_text(text)
        self.git("add", ".")
        self.git("commit", "-qm", "fixture")
        self.first = self.git("rev-parse", "HEAD")
        self.git("remote", "add", "origin", str(self.controller))
        subprocess.run(["git", "clone", "-q", "--no-hardlinks", str(self.controller),
                        str(self.base / "checkout")], check=True)
        self.configure(self.first)
        self.provision()

    def git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.controller), *args], text=True,
                                       stderr=subprocess.PIPE).strip()

    def configure(self, commit):
        (self.base / "settings.json").write_text(json.dumps({"schema": 1, "required_commit": commit}))

    def provision(self):
        path = self.base / "checkout/e2e/node_modules"
        (path / "@playwright/test").mkdir(parents=True, exist_ok=True)
        (path / "@playwright/test/cli.js").write_text("fixture external toolchain")
        digest = hashlib.sha256((self.controller / "e2e/package-lock.json").read_bytes()).hexdigest()
        (path / ".storyhook-lock-sha256").write_text(digest)

    def record(self):
        return json.loads((self.base / "latest.json").read_text())

    def commit_change(self, name, text):
        (self.controller / name).write_text(text)
        self.git("add", ".")
        self.git("commit", "-qm", "change")
        commit = self.git("rev-parse", "HEAD")
        # A controller is pinned while the remote dev branch advances.
        self.git("checkout", "-q", "--detach", self.first)
        return commit

    def test_success_unchanged_tip_runs_again_and_retains_logs(self):
        self.assertEqual(watcher.watch(self.base), 0)
        first = self.record()
        self.assertEqual(watcher.status(self.base)[0], "successful")
        self.assertEqual(watcher.watch(self.base), 0)
        self.assertNotEqual(first["log"], self.record()["log"])
        self.assertTrue(Path(first["log"]).is_file())
        self.assertEqual(self.record()["commit"], self.first)

    def test_new_tip_runs_and_dirty_checkout_is_preserved(self):
        new = self.commit_change("e2e/package-lock.json", '{"new":true}\n')
        self.assertNotEqual(watcher.watch(self.base), 0)
        self.assertIn("toolchain", self.record()["error"])
        # New lock is now checked out; explicitly provision it.
        path = self.base / "checkout/e2e"
        (path / "node_modules/.storyhook-lock-sha256").write_text(
            hashlib.sha256((path / "package-lock.json").read_bytes()).hexdigest())
        self.assertEqual(watcher.watch(self.base), 0)
        self.assertEqual(self.record()["commit"], new)
        (path / "package-lock.json").write_text("edited")
        self.assertNotEqual(watcher.watch(self.base), 0)
        self.assertIn("tracked edits", self.record()["error"])
        self.assertEqual((path / "package-lock.json").read_text(), "edited")

    def test_pending_integration_does_not_run_proof(self):
        self.git("checkout", "-qb", "feature")
        (self.controller / "feature").write_text("unmerged")
        self.git("add", ".")
        self.git("commit", "-qm", "unmerged")
        self.configure(self.git("rev-parse", "HEAD"))
        self.assertNotEqual(watcher.watch(self.base), 0)
        self.assertEqual(watcher.status(self.base)[0], "awaiting integration")
        self.assertNotIn("proof-executed", Path(self.record()["log"]).read_text())

    def test_failed_fetch_and_proof_replace_previous_success(self):
        self.assertEqual(watcher.watch(self.base), 0)
        self.git("remote", "set-url", "origin", str(self.base / "missing"))
        self.assertNotEqual(watcher.watch(self.base), 0)
        self.assertEqual(watcher.status(self.base)[0], "failed")

    def test_process_failure_and_missing_receipt_never_pass(self):
        for script in ("exit 1\n", "exit 0\n"):
            with self.subTest(script=script):
                self.git("checkout", "-q", "dev")
                self.commit_change("scripts/run-e2e.sh", script)
                self.assertNotEqual(watcher.watch(self.base), 0)
                self.assertEqual(watcher.status(self.base)[0], "failed")

    def test_lock_contention_does_not_overwrite_current_evidence(self):
        self.assertEqual(watcher.watch(self.base), 0)
        before = (self.base / "latest.json").read_bytes()
        with (self.base / "watch.lock").open("a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            self.assertEqual(watcher.watch(self.base), 0)
        self.assertEqual((self.base / "latest.json").read_bytes(), before)

    def test_status_missing_stale_corrupt_and_interrupted(self):
        self.assertEqual(watcher.status(self.base)[0], "missing")
        self.assertEqual(watcher.watch(self.base), 0)
        record = self.record()
        self.assertEqual(watcher.status(self.base, now=record["finished"] + 172801)[0], "stale")
        record.update(state="running", pid=99999999, process_started="gone", finished=None)
        (self.base / "latest.json").write_text(json.dumps(record))
        self.assertEqual(watcher.status(self.base)[0], "failed")
        (self.base / "latest.json").write_text("{}")
        self.assertEqual(watcher.status(self.base)[0], "failed")

    def test_plist_is_daily_durable_and_never_starts_proof_at_install(self):
        # This is a renderer contract, not a dependency on the user's installed tools.
        durable = "/opt/storyhook-isolation-fixture/bin"
        tools = ("story", "git", "cargo", "node", "npm")
        transient = str(ROOT / "scripts/python-bin") + os.pathsep + str(self.base) + os.pathsep + "."
        with (mock.patch.dict(os.environ, {"PATH": transient + os.pathsep + durable}),
              mock.patch.object(watcher.shutil, "which", side_effect=[
                  str(Path(durable) / tool) for tool in tools]) as lookup):
            plist = plistlib.loads(watcher.launchd(self.base))
        self.assertEqual(lookup.call_args_list,
                         [mock.call(tool, path=durable) for tool in tools])
        self.assertEqual(plist["StartCalendarInterval"], {"Hour": 4, "Minute": 17})
        self.assertFalse(plist.get("RunAtLoad", False))
        self.assertEqual(plist["EnvironmentVariables"]["STORYHOOK_ISOLATION_HOME"], str(self.base))
        self.assertIn(str(self.controller / "scripts/e2e-isolation-watch.sh"), plist["ProgramArguments"])
        path = plist["EnvironmentVariables"]["PATH"].split(os.pathsep)
        self.assertNotIn(str(ROOT / "scripts/python-bin"), path)
        self.assertNotIn(str(self.base), path)
        self.assertNotIn(".", path)
        self.assertEqual(path, [durable])

    def test_plist_refuses_each_missing_durable_tool(self):
        durable = "/opt/storyhook-isolation-fixture/bin"
        tools = ("story", "git", "cargo", "node", "npm")
        for index, missing in enumerate(tools):
            with (self.subTest(tool=missing),
                  mock.patch.dict(os.environ, {"PATH": durable}),
                  mock.patch.object(watcher.shutil, "which", side_effect=[
                      str(Path(durable) / tool) for tool in tools[:index]] + [None]) as lookup):
                with self.assertRaisesRegex(ValueError,
                                            f"^durable LaunchAgent PATH cannot find {missing}$"):
                    watcher.launchd(self.base)
                self.assertEqual(lookup.call_args_list,
                                 [mock.call(tool, path=durable) for tool in tools[:index + 1]])

    def test_fetched_new_tree_marks_prior_success_stale(self):
        self.assertEqual(watcher.watch(self.base), 0)
        self.commit_change("e2e/package-lock.json", '{"different":true}\n')
        subprocess.run(["git", "-C", str(self.base / "checkout"), "fetch", "-q", "origin", "dev"], check=True)
        self.assertEqual(watcher.status(self.base)[0], "stale")

    def test_missing_artifacts_and_changed_controller_refuse_success(self):
        self.assertEqual(watcher.watch(self.base), 0)
        (Path(self.record()["artifacts"]) / "isolation.json").unlink()
        self.assertEqual(watcher.status(self.base)[0], "failed")
        (self.controller / "scripts/branch-policy.sh").write_text("STORYHOOK_INTEGRATION_BRANCH=main\n")
        self.assertNotEqual(watcher.watch(self.base), 0)
        self.assertIn("controller has tracked edits", self.record()["error"])

    def test_cancellation_reaches_proof_and_waits_for_its_cleanup(self):
        script = self.base / "checkout/scripts/run-e2e.sh"
        script.write_text('trap \'sleep 600 & echo CLEANED; exit 143\' TERM\nsleep 600 &\necho READY\nwait\n')
        worker = '''import importlib.util, pathlib, sys
spec = importlib.util.spec_from_file_location("watcher", sys.argv[1])
watcher = importlib.util.module_from_spec(spec)
spec.loader.exec_module(watcher)
print("RESULT=" + str(watcher.run_proof(pathlib.Path(sys.argv[2]), pathlib.Path(sys.argv[3]), sys.stdout)), flush=True)
'''
        patience = grace.patience(30, grace.contention())
        with subprocess.Popen([sys.executable, "-B", "-c", worker,
                               str(ROOT / "scripts/e2e-isolation-watch.py"),
                               str(self.base / "checkout"), str(self.base / "artifacts")],
                              stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True) as child:
            try:
                with selectors.DefaultSelector() as selector:
                    selector.register(child.stdout, selectors.EVENT_READ)
                    self.assertTrue(selector.select(patience), "proof never became ready")
                    self.assertEqual(child.stdout.readline().strip(), "READY")
                child.send_signal(signal.SIGTERM)
                output, _ = child.communicate(timeout=patience)
                self.assertIn("CLEANED", output)
                self.assertIn("RESULT=143", output)
                self.assertEqual(child.returncode, 0)
            finally:
                if child.poll() is None:
                    child.terminate()
                    child.wait(timeout=patience)


if __name__ == "__main__":
    unittest.main()
