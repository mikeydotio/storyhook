"""Production observer contracts with real Git and an external preflight fixture."""
import importlib.util
import json
import os
from pathlib import Path
import plistlib
import selectors
import subprocess
import tempfile
import time
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("observer", ROOT / "scripts/release-observer.py")
observer = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(observer)


class ObserverTests(unittest.TestCase):
    """Each case owns its repository, remote, records, and command fixture."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="release-observer-", dir="/tmp")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name) / "repo"
        self.root.mkdir()
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.email", "test@example.com")
        self.git("config", "user.name", "Test")
        self.write("VERSION", "v1.0.0\n")
        self.write("scripts/build-release-assets.sh", '#!/bin/bash\n[ "$1" = --check ] || exit 91\necho probe\nexit 0\n')
        self.first = self.commit()
        self.git("tag", "v1.0.0")
        self.git("remote", "add", "origin", str(self.root))
        self.git("fetch", "-q", "origin", "main")
        self.state = self.root / ".git/storyhook/release-observer"

    def git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.root), *args], stderr=subprocess.PIPE, text=True).strip()

    def write(self, name, value):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(value)

    def commit(self):
        self.git("add", ".")
        self.git("commit", "-qm", "fixture")
        return self.git("rev-parse", "HEAD")

    def run_watch(self):
        return observer.watch(self.root)

    def record(self):
        return json.loads((self.state / "latest.json").read_text())

    def test_correct_tags_and_unchanged_tree_runs_again(self):
        self.git("tag", "-d", "v1.0.0")
        self.git("tag", "-a", "v1.0.0", "-m", "annotated")
        self.assertEqual(self.run_watch(), 0)
        first_log = self.record()["log"]
        self.assertEqual(self.run_watch(), 0)
        self.assertNotEqual(first_log, self.record()["log"])
        self.assertEqual(observer.status(self.root)[0], "successful")

    def test_wrong_merge_tag_does_not_skip_preflight(self):
        self.git("switch", "-qc", "release")
        self.write("VERSION", "v1.1.0\n")
        expected = self.commit()
        self.git("switch", "-q", "main")
        self.git("merge", "-q", "--no-ff", "release", "-m", "merge")
        actual = self.git("rev-parse", "HEAD")
        self.git("tag", "v1.1.0")
        self.assertEqual(self.run_watch(), 1)
        record = self.record()
        self.assertEqual(record["preflight"], 0)
        log = Path(record["log"]).read_text()
        self.assertIn(expected, log)
        self.assertIn(actual, log)
        self.assertIn("probe", log)

    def test_history_mismatch_missing_version_and_noncommit(self):
        self.write("VERSION", "v2.0.0\n")
        self.commit()
        self.git("tag", "v2.0.0")
        self.git("tag", "ignored")
        self.assertEqual(self.run_watch(), 0)
        self.git("tag", "v9.0.0")
        blob = self.git("rev-parse", "HEAD:VERSION")
        self.git("tag", "v8.0.0", blob)
        self.git("rm", "VERSION")
        self.commit()
        self.git("tag", "v7.0.0")
        self.assertEqual(self.run_watch(), 1)
        log = Path(self.record()["log"]).read_text()
        for tag in ("v9.0.0", "v8.0.0", "v7.0.0"):
            self.assertIn(tag, log)

    def test_remote_changes_do_not_change_local_tags(self):
        self.assertEqual(self.run_watch(), 0)
        clone = self.state / "checkout"
        self.assertEqual(subprocess.check_output(["git", "-C", str(clone), "tag"], text=True), "")
        self.write("README", "later")
        later = self.commit()
        self.git("tag", "-f", "v1.0.0", later)
        self.assertEqual(self.run_watch(), 1)
        self.git("tag", "-d", "v1.0.0")
        self.assertEqual(self.run_watch(), 0)
        refs = subprocess.check_output(["git", "-C", str(clone), "for-each-ref", "refs/release-observer/tags"], text=True)
        self.assertEqual(refs, "")

    def test_new_failure_supersedes_success_and_remote_failure_is_loud(self):
        self.assertEqual(self.run_watch(), 0)
        self.write("scripts/build-release-assets.sh", "#!/bin/bash\necho broken >&2\nexit 23\n")
        self.commit()
        self.assertEqual(self.run_watch(), 1)
        self.assertEqual(self.record()["preflight"], 23)
        self.assertEqual(observer.status(self.root)[0], "failed")
        self.git("remote", "set-url", "origin", str(self.root / "absent"))
        self.assertEqual(self.run_watch(), 1)
        self.assertEqual(observer.status(self.root)[0], "failed")

    def test_status_missing_running_interrupted_malformed_and_stale(self):
        self.assertEqual(observer.status(self.root)[0], "missing")
        self.assertEqual(self.run_watch(), 0)
        record = self.record()
        record["finished"] = int(time.time()) - observer.STALE_SECONDS - 1
        record["started"] = record["finished"] - 1
        observer.save(self.state, record)
        self.assertEqual(observer.status(self.root)[0], "stale")
        record["finished"] = int(time.time())
        record["tree"] = "0" * 40
        observer.save(self.state, record)
        self.assertEqual(observer.status(self.root)[0], "stale")
        record["state"] = "running"
        record["pid"] = os.getpid()
        record["process_started"] = observer.process_started(os.getpid())
        observer.save(self.state, record)
        self.assertEqual(observer.status(self.root)[0], "running")
        record["process_started"] = "not this process"
        observer.save(self.state, record)
        self.assertEqual(observer.status(self.root)[0], "failed")
        (self.state / "latest.json").write_text("{partial")
        self.assertEqual(observer.status(self.root)[0], "failed")
        (self.state / "latest.json").write_text('{}')
        self.assertEqual(observer.status(self.root)[0], "failed")

    def test_invalid_success_metadata_never_reports_success(self):
        self.assertEqual(self.run_watch(), 0)
        original = self.record()
        for key, value in (("commit", None), ("tree", []), ("audit", False),
                           ("finished", "yesterday"), ("schema", True)):
            with self.subTest(key=key):
                observer.save(self.state, dict(original, **{key: value}))
                self.assertEqual(observer.status(self.root)[0], "failed")

    def test_partial_record_write_preserves_previous_complete_record(self):
        self.assertEqual(self.run_watch(), 0)
        original = (self.state / "latest.json").read_bytes()
        with mock.patch.object(observer.json, "dump", side_effect=OSError("disk full")):
            with self.assertRaises(OSError):
                observer.save(self.state, {"partial": True})
        self.assertEqual((self.state / "latest.json").read_bytes(), original)

    def test_status_wrapper_is_read_only_and_advisory_preserves_exit(self):
        command = 'bash "$1" || true; exit "$2"'
        for code in (0, 23):
            result = subprocess.run(["bash", "-c", command, "advisory",
                                     str(ROOT / "scripts/release-status.sh"), str(code)],
                                    cwd=self.root, capture_output=True, text=True)
            self.assertEqual(result.returncode, code)
            self.assertIn("MISSING", result.stderr)
            self.assertFalse(self.state.exists())

    def test_launchd_escapes_paths_and_refuses_linked_lane(self):
        value = plistlib.loads(observer.launchd(self.root))
        self.assertEqual(value["StartCalendarInterval"], {"Minute": 0})
        self.assertTrue(value["RunAtLoad"])
        self.assertTrue(Path(value["ProgramArguments"][0]).is_absolute())
        lane = Path(self.tmp.name) / "lane"
        self.git("worktree", "add", "-q", "--detach", str(lane))
        with self.assertRaises(ValueError):
            observer.launchd(lane)

    def test_dirty_observer_checkout_is_not_reported_as_checked_commit(self):
        self.assertEqual(self.run_watch(), 0)
        probe = self.state / "checkout/scripts/build-release-assets.sh"
        probe.write_text("#!/bin/bash\necho untracked-behavior\nexit 0\n")
        self.assertEqual(self.run_watch(), 1)
        self.assertEqual(observer.status(self.root)[0], "failed")

    def test_concurrent_public_passes_serialize(self):
        self.write("scripts/build-release-assets.sh", '''#!/bin/bash
set -eu
mkdir "$OBSERVER_TEST_EVENTS/active"
trap 'rmdir "$OBSERVER_TEST_EVENTS/active"' EXIT
echo entered >> "$OBSERVER_TEST_EVENTS/entries"
while [ ! -e "$OBSERVER_TEST_EVENTS/release" ]; do sleep 0.05; done
''')
        self.commit()
        events = Path(self.tmp.name) / "events"
        events.mkdir()
        env = dict(os.environ, OBSERVER_TEST_EVENTS=str(events),
                   STORYHOOK_LOCK_DIR=str(events / "locks"))
        env.pop("STORYHOOK_MACHINE_LOCKS", None)
        command = ["bash", str(ROOT / "scripts/release-watch.sh")]
        first = subprocess.Popen(command, cwd=self.root, env=env, stderr=subprocess.PIPE, text=True)
        second = None
        try:
            deadline = time.monotonic() + 30
            while not (events / "entries").exists():
                self.assertIsNone(first.poll(), "first pass exited before preflight")
                self.assertLess(time.monotonic(), deadline, "fixture failed to enter preflight")
                time.sleep(0.05)
            second = subprocess.Popen(command, cwd=self.root, env=env, stderr=subprocess.PIPE, text=True)
            with selectors.DefaultSelector() as selector:
                selector.register(second.stderr, selectors.EVENT_READ)
                self.assertTrue(selector.select(30), "second pass did not report its lock wait")
                self.assertIn("waiting", second.stderr.readline().lower())
            self.assertEqual((events / "entries").read_text().splitlines(), ["entered"])
            (events / "release").touch()
            self.assertEqual(first.communicate(timeout=30)[0], None)
            self.assertEqual(second.communicate(timeout=30)[0], None)
            self.assertEqual(first.returncode, 0)
            self.assertEqual(second.returncode, 0)
            self.assertEqual((events / "entries").read_text().splitlines(), ["entered", "entered"])
        finally:
            (events / "release").touch()
            for process in (first, second):
                if process is not None:
                    if process.poll() is None:
                        process.terminate()
                    process.communicate(timeout=30)


if __name__ == "__main__":
    unittest.main()
