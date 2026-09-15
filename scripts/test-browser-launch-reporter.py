"""Exercise browser failure handling through the real Playwright runner."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
PLAYWRIGHT = ROOT / "e2e/node_modules/@playwright/test"
REPORTER = ROOT / "e2e/browser-launch-reporter.ts"


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
            result = subprocess.run(
                ["node", str(PLAYWRIGHT / "cli.js"), "test", "--config", str(root / "playwright.config.cjs")],
                cwd=root, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                timeout=60,
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
    unittest.main()
