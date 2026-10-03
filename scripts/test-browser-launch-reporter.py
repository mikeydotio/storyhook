"""Exercise browser failure handling through the real Playwright runner."""

import json
import os
from pathlib import Path
import select
import signal
import subprocess
import sys
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parent.parent
PLAYWRIGHT = ROOT / "e2e/node_modules/@playwright/test"
REPORTER = ROOT / "e2e/browser-launch-reporter.ts"
# The Node owner derives its budget from this bound and the four cases below.
CASE_TIMEOUT_SECONDS = 60
# Responsiveness cadence, not a deadline or an allowance for machine speed.
CANCELLATION_POLL_SECONDS = 0.1
WATCH_PARENT = False


class ReporterCancelled(BaseException):
    """Owner cancellation must unwind cleanup without unittest starting another case."""


def run_owned(command, *, timeout, cwd=None, env=None, watch_parent=False):
    """Run a nested reporter process and retain ownership until it is reaped."""
    cancelled = None

    def cancel(signum, _frame):
        """Do not throw between the OS spawn and publication of its process handle."""
        nonlocal cancelled
        cancelled = f"signal {signum}"

    previous = {sig: signal.signal(sig, cancel) for sig in (signal.SIGTERM, signal.SIGINT)}
    child = None
    failure = None
    try:
        child = subprocess.Popen(command, cwd=cwd, env=env, text=True,
                                 stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                 start_new_session=True)
        deadline = time.monotonic() + timeout
        while True:
            if cancelled is not None:
                raise ReporterCancelled(f"reporter runner cancelled by {cancelled}: {command}")
            if watch_parent and select.select([sys.stdin], [], [], 0)[0]:
                # The owner writes no input. EOF therefore identifies its death.
                if os.read(sys.stdin.fileno(), 1) == b"":
                    raise ReporterCancelled(f"reporter owner pipe closed: {command}")
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise subprocess.TimeoutExpired(command, timeout)
            if child.poll() is not None:
                break
            try:
                child.communicate(timeout=min(remaining, CANCELLATION_POLL_SECONDS))
            except subprocess.TimeoutExpired:
                continue
            break
        if cancelled is not None:
            raise ReporterCancelled(f"reporter runner cancelled by {cancelled}: {command}")
    except BaseException as error:
        failure = error
        raise
    finally:
        try:
            if child is not None:
                # A runner may exit while a worker still owns its pipes. Signal the
                # private group even on success, before draining inherited handles.
                try:
                    os.killpg(child.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass  # Absence is the successful terminal state, not a lost error.
                output, _ = child.communicate()
                if failure is not None:
                    failure.add_note(f"nested reporter output:\n{output}")
        finally:
            for sig, handler in previous.items():
                signal.signal(sig, handler)
    return subprocess.CompletedProcess(command, child.returncode, output)


class BrowserLaunchReporterTests(unittest.TestCase):
    """A failed browser launch must stop its project without hiding ordinary failures."""

    def run_case(self, first_body, browser=False):
        """Run two real tests with an unavailable executable as the failure boundary."""
        with tempfile.TemporaryDirectory(prefix="SH-733-browser-", dir="/tmp") as scratch:
            root = Path(scratch)
            sentinel = root / "second-ran"
            config = {
                "testDir": str(root),
                "workers": 1,
                "retries": 0,
                "reporter": [["list"], [str(REPORTER)]],
                "use": {"launchOptions": {"executablePath": str(root / "absent-browser")}},
            }
            (root / "playwright.config.cjs").write_text("module.exports = " + json.dumps(config))
            (root / "launch.spec.cjs").write_text(
                "const {test, expect} = require(" + json.dumps(str(PLAYWRIGHT)) + ");\n"
                "const fs = require('node:fs');\n"
                "test('first', async (" + ("{page}" if browser else "{}") + ") => {"
                + first_body + "});\n"
                "test('second', async () => {fs.writeFileSync("
                + json.dumps(str(sentinel)) + ", 'ran');});\n"
            )
            env = dict(os.environ, CI="1")
            env.pop("NO_COLOR", None)
            for key in ("STORYHOOK_GATE_PROGRESS", "STORYHOOK_GATE_PROGRESS_PATH"):
                env.pop(key, None)
            result = run_owned(
                ["node", str(PLAYWRIGHT / "cli.js"), "test", "--config", str(root / "playwright.config.cjs")],
                cwd=root, env=env, timeout=CASE_TIMEOUT_SECONDS, watch_parent=WATCH_PARENT,
            )
            return result, sentinel.exists()

    def test_browser_launch_failure_stops_remaining_tests(self):
        """A real missing executable fails once and never reaches the second test."""
        result, second_ran = self.run_case("await page.goto('about:blank');", browser=True)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("browserType.launch", result.stdout)
        self.assertFalse(second_ran, result.stdout)
        self.assertIn("browser-launch: stopping project", result.stdout)

    def test_ordinary_failure_does_not_stop_remaining_tests(self):
        """Assertion failures retain the release suite's complete enumeration."""
        result, second_ran = self.run_case("expect(1).toBe(2);")
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertTrue(second_ran, result.stdout)
        self.assertNotIn("browser-launch: stopping project", result.stdout)

    def test_success_does_not_interrupt_the_runner(self):
        """A passing project keeps its normal successful exit status."""
        result, second_ran = self.run_case("expect(1).toBe(1);")
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertTrue(second_ran, result.stdout)

    def test_assertion_that_mentions_launch_is_not_a_launch_failure(self):
        """An assertion quoting launch output remains an ordinary test failure."""
        result, second_ran = self.run_case("expect('browserType.launch: example').toBe('different');")
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertTrue(second_ran, result.stdout)
        self.assertNotIn("browser-launch: stopping project", result.stdout)


if __name__ == "__main__":
    if "--watch-parent" in sys.argv:
        sys.argv.remove("--watch-parent")
        WATCH_PARENT = True
    try:
        unittest.main()
    except ReporterCancelled as error:
        print(error, file=sys.stderr)
        for note in getattr(error, "__notes__", []):
            print(note, file=sys.stderr)
        sys.exit(1)
