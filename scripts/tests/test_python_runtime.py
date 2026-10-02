"""SH-858: interpreter selection is policy, never inherited PATH ordering."""

import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile
import unittest
import venv


SCRIPTS = Path(__file__).resolve().parents[1]
RUNTIME = SCRIPTS / "python-runtime.sh"


class RuntimeTests(unittest.TestCase):
    """Exercise real shell entry points with controlled interpreter endpoints."""

    def setUp(self):
        """Give each case its own executable and artifact directory."""
        self.temp = tempfile.TemporaryDirectory(prefix="sh858-", dir="/tmp")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.env = dict(os.environ)
        for name in ("STORYHOOK_PYTHON", "STORYHOOK_PYTHON_PROBE"):
            self.env.pop(name, None)
        self.env["PYTHONDONTWRITEBYTECODE"] = "1"

    def executable(self, name, body):
        """Create an endpoint; runtime behavior outside discovery remains real."""
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("#!/bin/bash\n" + body + "\n")
        path.chmod(0o755)
        return path

    def shell(self, body, *args):
        """Source the production policy without modifying its candidate list."""
        return subprocess.run(
            ["/bin/bash", "-eu", "-c", '. "$1"; shift; ' + body,
             "runtime-test", str(RUNTIME), *map(str, args)],
            env=self.env, text=True, capture_output=True, check=False,
        )

    def test_path_order_cannot_choose_the_runtime(self):
        """A poison executable on PATH must never be probed or executed."""
        marker = self.root / "poison-ran"
        poison = self.executable("poison/python3", f"touch {shlex.quote(str(marker))}; exit 91")
        answers = []
        for path in (f"{poison.parent}:/usr/bin:/bin:/opt/homebrew/bin:/usr/local/bin",
                     f"/opt/homebrew/bin:/usr/local/bin:{poison.parent}:/usr/bin:/bin"):
            self.env["PATH"] = path
            result = self.shell('storyhook_python_init; python3 -c "import sys; print(sys.executable)"')
            self.assertEqual(result.returncode, 0, result.stderr)
            answers.append(result.stdout)
        self.assertEqual(answers[0], answers[1])
        self.assertFalse(marker.exists())

    def test_discovery_skips_unsupported_and_keeps_order(self):
        """Unsupported endpoints cannot shadow the first supported candidate."""
        old = self.executable("old", "printf '3.9.6\\n'; exit 2")
        good = self.executable("good", f"exec {shlex.quote(sys.executable)} \"$@\"")
        unused = self.executable("unused", "exit 91")
        result = self.shell('storyhook_python_select "$@"', old, good, unused)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(Path(result.stdout.strip()).resolve(), Path(sys.executable).resolve())

    def test_discovery_reports_every_failed_candidate(self):
        """No compatible runtime is an explained refusal, never a fallback."""
        old = self.executable("old", "printf '3.9.6\\n'; exit 2")
        broken = self.executable("broken", "printf 'garbage\\n'; exit 0")
        missing = self.root / "missing"
        result = self.shell('storyhook_python_select "$@"', old, broken, missing)
        self.assertNotEqual(result.returncode, 0)
        for evidence in ("3.11", "3.9.6", str(old), str(broken), str(missing), "STORYHOOK_PYTHON"):
            self.assertIn(evidence, result.stderr)
        self.assertEqual(result.stdout, "")

    def test_version_boundary_is_explicit(self):
        """Version validation excludes Python 2, old Python 3 and future majors."""
        for version, accepted in (("2.7.18", False), ("3.9.6", False),
                                  ("3.10.22", False), ("3.11.0", True),
                                  ("3.14.7", True), ("4.0.0", False)):
            with self.subTest(version=version):
                candidate = self.executable("version", "printf '%s\\n' " +
                                            shlex.quote(version) + " " + shlex.quote(sys.executable))
                result = self.shell('storyhook_python_select "$1"', candidate)
                self.assertEqual(result.returncode == 0, accepted, result.stderr)

    def test_bad_overrides_never_fall_back(self):
        """Relative, absent, non-executable and malformed overrides fail loud."""
        nonexec = self.root / "nonexec"
        nonexec.write_text("not executable")
        malformed = self.executable("malformed", "printf '3.14.7\\nrelative\\n'")
        for value in ("python3", str(self.root / "missing"), str(nonexec), str(malformed), ""):
            with self.subTest(value=value):
                self.env["STORYHOOK_PYTHON"] = value
                result = self.shell('storyhook_python_init || { printf "%s\\n" "$STORYHOOK_PYTHON_ERROR" >&2; exit 2; }')
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("3.11", result.stderr)

    def test_launcher_cannot_be_selected_directly_or_through_a_symlink(self):
        """A mistaken override must not recurse indefinitely."""
        link = self.root / "linked-python"
        link.symlink_to(SCRIPTS / "python-bin/python3")
        for path in (SCRIPTS / "python-bin/python3", link):
            self.env["STORYHOOK_PYTHON"] = str(path)
            result = self.shell('storyhook_python_init || { printf "%s\\n" "$STORYHOOK_PYTHON_ERROR" >&2; exit 2; }')
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("launcher", result.stderr)

    def test_override_pins_real_runtime_and_preserves_children_and_arguments(self):
        """Spaces, shebangs and nested Python launches preserve interpreter identity."""
        runtime = self.root / "runtime with spaces/python"
        runtime.parent.mkdir()
        runtime.symlink_to(sys.executable)
        self.env["STORYHOOK_PYTHON"] = str(runtime)
        child = self.root / "child"
        child.write_text('#!/usr/bin/env python3\nimport json,sys,subprocess\n'
                         'print(json.dumps([sys.executable,sys.argv[1:],'
                         'subprocess.check_output([sys.executable,"-c","import sys; print(sys.executable)"],text=True).strip()]))\n')
        child.chmod(0o755)
        result = self.shell('storyhook_python_init; "$1" "two words" ""', child)
        self.assertEqual(result.returncode, 0, result.stderr)
        executable, args, nested = json.loads(result.stdout)
        self.assertEqual(Path(executable).resolve(), Path(sys.executable).resolve())
        self.assertEqual(nested, executable)
        self.assertEqual(args, ["two words", ""])
        result = self.shell('storyhook_python_init; python3 -c "raise SystemExit(17)"')
        self.assertEqual(result.returncode, 17)

    def test_virtual_environment_override_keeps_its_environment(self):
        """Resolving a venv symlink must not silently discard its environment."""
        environment = self.root / "virtual environment"
        venv.EnvBuilder(with_pip=False, symlinks=True).create(environment)
        self.env["STORYHOOK_PYTHON"] = str(environment / "bin/python3")
        result = self.shell('storyhook_python_init; python3 -c "import sys; print(sys.prefix)"')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(Path(result.stdout.strip()).resolve(), environment.resolve())

    def test_verifier_refuses_before_git_or_gate_activity(self):
        """The real entry point returns infrastructure JSON even outside a repo."""
        old = self.executable("old", "printf '3.9.6\\n'; exit 2")
        self.env["STORYHOOK_PYTHON"] = str(old)
        result = subprocess.run(["/bin/bash", str(SCRIPTS / "verify-pr.sh"), "--run-gate"],
                                cwd=self.root, env=self.env, text=True, capture_output=True, check=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        verdict = json.loads(result.stdout)
        self.assertEqual(verdict["result"], "infrastructure-failure")
        self.assertEqual(verdict["disposition"], "permanent")
        for evidence in (str(old), "3.9.6", "3.11"):
            self.assertIn(evidence, verdict["detail"])
        self.assertEqual(set(self.root.iterdir()), {old})

    def test_all_entry_points_refuse_an_unsupported_runtime(self):
        """Every independent entrance validates before invoking its work."""
        old = self.executable("old", "printf '3.9.6\\n'; exit 2")
        self.env["STORYHOOK_PYTHON"] = str(old)
        entries = {
            "verify-pr.sh": (["--landing", "attempt", "1", "head", "tree", str(self.root / "marker")], "not-attempted"),
            "verify-batch.sh": (["base-policy", "main"], "python-runtime"),
            "merge-preflight.sh": (["--json", "HEAD", "HEAD"], "inspection-error"),
            "landing-intent.sh": (["attempt", "1", "head", "tree", str(self.root / "marker")], "not-attempted"),
            "merge-watch.sh": ([], None),
            "land-pr.sh": (["1"], None),
            "run-tests.sh": (["--only"], None),
            "run-rust-battery.sh": (["contracts"], None),
            "run-changed.sh": ([], None),
            "run-e2e.sh": ([], None),
            "../plugins/story/tests/run-tests.sh": (["nonexistent-test"], None),
        }
        for entry, (args, verdict) in entries.items():
            with self.subTest(entry=entry):
                result = subprocess.run(["/bin/bash", str(SCRIPTS / entry), *args],
                                        cwd=SCRIPTS.parent, env=self.env, text=True,
                                        capture_output=True, check=False)
                if verdict:
                    data = json.loads(result.stdout)
                    self.assertEqual(data.get("result", data.get("reason")), verdict, data)
                else:
                    self.assertNotEqual(result.returncode, 0)
                self.assertIn(str(old), result.stdout + result.stderr)
                self.assertIn("3.11", result.stdout + result.stderr)
        self.assertFalse((self.root / "marker").exists())

    def test_landing_recovery_never_claims_an_old_request_was_not_attempted(self):
        """Missing runtime cannot authorize replay of an earlier landing request."""
        self.env["STORYHOOK_PYTHON"] = str(self.root / "missing-python")
        marker = self.root / "attempt-marker"
        for mode, prior_attempt in (("recover", False), ("attempt", True), ("recover", True)):
            with self.subTest(mode=mode, prior_attempt=prior_attempt):
                if prior_attempt:
                    marker.write_text("retained evidence")
                result = subprocess.run(
                    ["/bin/bash", str(SCRIPTS / "verify-pr.sh"), "--landing",
                     mode, "1", "head", "tree", str(marker)],
                    cwd=self.root, env=self.env, text=True, capture_output=True, check=False,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                verdict = json.loads(result.stdout)
                self.assertEqual(verdict["result"], "uncertain", verdict)
                self.assertIn("3.11", verdict["detail"])
                self.assertEqual(marker.exists(), prior_attempt)

    def test_runtime_environment_does_not_reorder_other_tools(self):
        """Only python3 gains a launcher; normal PATH behavior is preserved."""
        tool = self.executable("tools/another-tool", "printf 'original tool\\n'")
        self.env["PATH"] = str(tool.parent) + ":" + self.env["PATH"]
        result = self.shell('storyhook_python_init; before="$PATH"; storyhook_python_init; '
                            '[ "$PATH" = "$before" ]; another-tool')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "original tool\n")

    def test_shell_git_scrubs_preserve_the_selected_runtime(self):
        """Both Git policies must retain the interpreter needed by PATH shims."""
        real_git = shutil.which("git")
        repository = self.root / "repository"
        repository.mkdir()
        subprocess.run([real_git, "init", "-q", str(repository)], check=True)
        wrapper = self.root / "tools/git"
        wrapper.parent.mkdir()
        wrapper.write_text('#!/usr/bin/env python3\nimport os,sys\n'
                           'assert os.path.samefile(sys.executable, os.environ["STORYHOOK_PYTHON"])\n'
                           f'os.execv({real_git!r}, [{real_git!r}, *sys.argv[1:]])\n')
        wrapper.chmod(0o755)
        self.env["PATH"] = f"{wrapper.parent}:{self.env['PATH']}"
        self.env["STORYHOOK_PYTHON"] = sys.executable
        for body in (
            '. "$2/test-env.sh"; storyhook_fixture_git -C "$1" status --porcelain',
            'cd "$1"; /bin/bash "$2/git-identity.sh" audit',
        ):
            with self.subTest(body=body):
                result = self.shell('storyhook_python_init; ' + body, repository, SCRIPTS)
                self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
