"""Production runners enter host admission once and keep their behaviour (SH-869).

These run the tracked runner scripts in a scratch checkout whose `cargo` is a
recording fake. The host authority is disabled there (no policy at the
canonical namespace), so each case also proves a disabled host changes
nothing but the admission marker.
"""

import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
from host_admit_fixture import PROCESS_ALLOWANCE_S

CHECKOUT = Path(__file__).resolve().parents[2]
POLICY = Path("/var/tmp/storyhook-host-admission-v1/policy.json")

FAKE_CARGO = """#!/bin/sh
printf '%s|entry=%s|threads=%s|units=%s\\n' "$*" "${STORYHOOK_HOST_ENTRY:-}" \\
    "${RUST_TEST_THREADS:-}" "${STORYHOOK_HOST_UNITS:-}" >> "$CARGO_LOG"
exit 0
"""


def runner_env(fixture, **extra):
    """The runner's environment: lane, lock and admission state of the caller removed."""
    env = {k: v for k, v in os.environ.items()
           if not k.startswith(("STORYHOOK_HOST_", "STORYHOOK_GATE_LOCK", "STORYHOOK_MACHINE_LOCKS"))
           and k not in ("STORYHOOK_AUTO", "STORYHOOK_DISPATCH", "STORYHOOK_FULL_AUTO",
                         "STORYHOOK_TEST_THREAD_BUDGET", "STORYHOOK_GATE_PROGRESS")}
    env.update(PATH=f"{fixture / 'bin'}:{os.environ['PATH']}", STORYHOOK_LOCK_DIR=str(fixture / "locks"),
               CARGO_LOG=str(fixture / "cargo.log"), STORYHOOK_PYTHON=sys.executable, **extra)
    return env


@unittest.skipIf(POLICY.exists(), "these cases prove the disabled host; this host has a policy")
class RustBattery(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(dir="/tmp", prefix="rtw-")
        self.addCleanup(self.tmp.cleanup)
        self.fixture = Path(self.tmp.name)
        for name in ("scripts", "bin", "tests", "locks"):
            (self.fixture / name).mkdir()
        # Symlinks, never copies: the point is to run the tracked scripts.
        for path in (CHECKOUT / "scripts").iterdir():
            if path.name != "test-delta.sh":
                (self.fixture / "scripts" / path.name).symlink_to(path)
        # No tree oid, so no ledger reaches the real repository.
        self.executable("scripts/test-delta.sh", "#!/bin/sh\nexit 1\n")
        self.executable("bin/cargo", FAKE_CARGO)
        subprocess.run(["git", "init", "-q"], cwd=self.fixture, check=True)

    def executable(self, relative, body):
        path = self.fixture / relative
        path.write_text(body)
        path.chmod(0o755)

    def run_battery(self, **extra):
        done = subprocess.run(["bash", "scripts/run-tests.sh"], cwd=self.fixture,
                              env=runner_env(self.fixture, **extra), capture_output=True, text=True,
                              stdin=subprocess.DEVNULL, timeout=PROCESS_ALLOWANCE_S * 4)
        self.assertEqual(done.returncode, 0, done.stderr)
        calls = (self.fixture / "cargo.log").read_text().splitlines()
        return calls, done.stderr

    def assert_admitted_once(self, calls):
        runs = [c for c in calls if c.startswith("test --no-fail-fast --workspace|")]
        self.assertEqual(len(runs), 1, f"the battery ran {len(runs)} times: {calls}")
        for call in calls:
            _, entry, threads, units = call.split("|")
            self.assertRegex(entry, r"^entry=rust-pool:\d+$", call)
            self.assertEqual(threads, "threads=", "a disabled host leaves libtest's threads alone")
            self.assertEqual(units, "units=", "the unit count is consumed, never inherited")

    def test_the_battery_is_admitted_inside_the_gate_lock_exactly_once(self):
        calls, _ = self.run_battery()
        self.assert_admitted_once(calls)

    def test_without_the_lock_the_battery_admits_itself_once_and_says_so_once(self):
        calls, stderr = self.run_battery(STORYHOOK_GATE_LOCK="0")
        self.assert_admitted_once(calls)
        self.assertEqual(stderr.count("WITHOUT this repository's 'gate' lock"), 1, stderr)



@unittest.skipIf(POLICY.exists(), "these cases prove the disabled host; this host has a policy")
class PluginRunner(unittest.TestCase):
    """The tracked plugin runner and lib.sh in a scratch plugin tree."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(dir="/tmp", prefix="ptw-")
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name)
        self.tests = root / "plugins" / "story" / "tests"
        self.tests.mkdir(parents=True)
        (root / "scripts").mkdir()
        for path in (CHECKOUT / "scripts").iterdir():
            (root / "scripts" / path.name).symlink_to(path)
        for name in ("run-tests.sh", "lib.sh"):
            (self.tests / name).symlink_to(CHECKOUT / "plugins" / "story" / "tests" / name)
        self.log = root / "seen.log"

    def script(self, name, body):
        path = self.tests / name
        path.write_text("#!/usr/bin/env bash\n" + body)
        path.chmod(0o755)
        return path

    def run_bash(self, *argv, **extra):
        env = runner_env(Path(self.tmp.name), SEEN=str(self.log), **extra)
        return subprocess.run(["bash", *argv], cwd=self.tmp.name, env=env, capture_output=True, text=True,
                              stdin=subprocess.DEVNULL, timeout=PROCESS_ALLOWANCE_S * 4)

    def test_the_pool_is_admitted_once_and_runs_its_requested_jobs(self):
        record = 'printf "%s|%s|%s\\n" "$(basename "$0")" "${STORYHOOK_HOST_ENTRY:-}" "${STORYHOOK_HOST_UNITS:-}" >> "$SEEN"\n'
        for name in ("test-alpha.sh", "test-beta.sh", "test-gamma.sh"):
            self.script(name, record)
        done = self.run_bash(str(self.tests / "run-tests.sh"), STORYHOOK_PLUGIN_JOBS="2")
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        seen = sorted(self.log.read_text().splitlines())
        self.assertEqual([line.split("|")[0] for line in seen], ["test-alpha.sh", "test-beta.sh", "test-gamma.sh"],
                         "each script ran exactly once")
        for line in seen:
            _, entry, units = line.split("|")
            self.assertRegex(entry, r"^plugin-pool:\d+$", "scripts run inside the pool's admission")
            self.assertEqual(units, "", "the unit count is consumed by the pool")
        self.assertIn("slowest (jobs=2)", done.stdout + done.stderr, "a disabled host keeps the requested jobs")

    def test_a_test_script_run_on_its_own_admits_itself_through_lib_sh(self):
        record = 'printf "%s\\n" "${STORYHOOK_HOST_ENTRY:-}" >> "$SEEN"\nsource "$(dirname "$0")/lib.sh"\n'
        direct = self.script("test-direct.sh", record + 'echo "after lib.sh"\n')
        self.run_bash(str(direct))
        first, second = self.log.read_text().splitlines()[:2]
        self.assertEqual(first, "", "the first pass ran unadmitted")
        self.assertRegex(second, r"^plugin-script:\d+$", "lib.sh re-executed the script through the adapter")
        self.log.unlink()
        helper = self.script("helper.sh", record)
        self.run_bash(str(helper))
        self.assertEqual(self.log.read_text().splitlines(), [""], "only test-*.sh scripts admit themselves")
        self.log.unlink()
        self.run_bash(str(direct), STORYHOOK_HOST_ENTRY="plugin-pool:1")
        self.assertEqual(self.log.read_text().splitlines()[0], "plugin-pool:1",
                         "inside an admitted entry the script runs in place")
        self.assertEqual(len(self.log.read_text().splitlines()), 1)



@unittest.skipIf(POLICY.exists(), "these cases prove the disabled host; this host has a policy")
class BrowserSuite(unittest.TestCase):
    """The tracked browser runner, stopped at its first build by a failing cargo."""

    def test_the_browser_suite_is_admitted_once_before_its_first_build(self):
        with tempfile.TemporaryDirectory(dir="/tmp", prefix="btw-") as tmp:
            fixture = Path(tmp)
            (fixture / "scripts").mkdir()
            (fixture / "bin").mkdir()
            for path in (CHECKOUT / "scripts").iterdir():
                (fixture / "scripts" / path.name).symlink_to(path)
            # The runner reads its project list from the tracked config first.
            (fixture / "e2e").symlink_to(CHECKOUT / "e2e")
            cargo = fixture / "bin" / "cargo"
            cargo.write_text(FAKE_CARGO.replace("exit 0", "exit 1"))
            cargo.chmod(0o755)
            subprocess.run(["git", "init", "-q"], cwd=fixture, check=True)
            done = subprocess.run(["bash", "scripts/run-e2e.sh"], cwd=fixture,
                                  env=runner_env(fixture, STORYHOOK_E2E_JOBS="3"), capture_output=True,
                                  text=True, stdin=subprocess.DEVNULL, timeout=PROCESS_ALLOWANCE_S * 4)
            calls = (fixture / "cargo.log").read_text().splitlines()
        self.assertNotEqual(done.returncode, 0, "the failing build stops the suite")
        self.assertEqual(len(calls), 1, f"one build, so one admitted pass: {calls}")
        _, entry, _, units = calls[0].split("|")
        self.assertRegex(entry, r"^entry=browser-pool:\d+$")
        self.assertEqual(units, "units=", "the unit count is consumed before any slice")



class VerifierWorkers(unittest.TestCase):
    """The nested Python case pool runs as its own admission entry."""

    def test_the_case_pool_re_executes_through_the_adapter_once(self):
        import run_verifier_tests as runner
        from unittest import mock

        def capture(program, argv):
            raise SystemExit(("exec", program, argv))

        env = {k: v for k, v in os.environ.items() if k != "STORYHOOK_HOST_ENTRY"}
        with mock.patch.dict(os.environ, env, clear=True), mock.patch("os.execv", capture), \
                mock.patch.object(sys, "argv", ["run_verifier_tests.py", "--jobs", "3", "lifecycle"]), \
                self.assertRaises(SystemExit) as replaced:
            runner.admitted_jobs(3)
        _, program, argv = replaced.exception.code
        self.assertEqual(program, sys.executable)
        self.assertEqual(argv[1:8], ["-B", str(CHECKOUT / "scripts" / "host-admit.py"), "--entry",
                                     "verifier-python-workers", "--units", "3", "--"])
        self.assertEqual(argv[8:], [sys.executable, "-B", str(Path(runner.__file__).resolve()),
                                    "--jobs", "3", "lifecycle"])
        admitted = {"STORYHOOK_HOST_ENTRY": f"verifier-python-workers:{os.getpid()}", "STORYHOOK_HOST_UNITS": "2"}
        with mock.patch.dict(os.environ, admitted), mock.patch("os.execv", capture):
            self.assertEqual(runner.admitted_jobs(3), 2, "inside its admission the pool runs the admitted count")
            self.assertNotIn("STORYHOOK_HOST_UNITS", os.environ, "the count is consumed")

    @unittest.skipIf(POLICY.exists(), "this case proves the disabled host; this host has a policy")
    def test_a_disabled_host_keeps_the_requested_jobs(self):
        case = "ContentionGrace.test_multiplier_is_exactly_one_without_contention"
        env = {k: v for k, v in os.environ.items() if not k.startswith("STORYHOOK_HOST_")}
        done = subprocess.run([sys.executable, "-B", str(Path(__file__).resolve().parent / "run_verifier_tests.py"),
                               "--jobs", "3", "lifecycle", case], env=env, capture_output=True, text=True,
                              stdin=subprocess.DEVNULL, timeout=PROCESS_ALLOWANCE_S * 4)
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertIn("selected=1 completed=1 failed=0 jobs=3", done.stdout)



class ReleasePaths(unittest.TestCase):
    """The release observer's preflight runs as a release-observer entry."""

    def test_the_observer_preflight_is_admitted(self):
        import importlib.util
        spec = importlib.util.spec_from_file_location("release_observer", CHECKOUT / "scripts" / "release-observer.py")
        observer = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(observer)
        self.assertEqual(observer.preflight_command(),
                         [sys.executable, "-B", str(CHECKOUT / "scripts" / "host-admit.py"), "--entry",
                          "release-observer", "--", "bash", "scripts/build-release-assets.sh", "--check"])

    @unittest.skipIf(POLICY.exists(), "this case proves the disabled host; this host has a policy")
    def test_a_disabled_host_leaves_the_observer_locks_grace_alone(self):
        with tempfile.TemporaryDirectory(dir="/tmp", prefix="rlw-") as tmp:
            scripts = Path(tmp) / "scripts"
            scripts.mkdir()
            for name in ("release-watch.sh", "host-admit.py"):
                (scripts / name).symlink_to(CHECKOUT / "scripts" / name)
            fake = scripts / "machine-lock.sh"
            fake.write_text('#!/usr/bin/env bash\nprintf "%s\\n" "$@"\n')
            fake.chmod(0o755)
            done = subprocess.run(["bash", str(scripts / "release-watch.sh")], capture_output=True, text=True,
                                  env=runner_env(Path(tmp)), stdin=subprocess.DEVNULL,
                                  timeout=PROCESS_ALLOWANCE_S)
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(done.stdout.splitlines()[:3], ["release-observer", "--", "python3"])


if __name__ == "__main__":
    unittest.main()
