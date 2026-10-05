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


if __name__ == "__main__":
    unittest.main()
