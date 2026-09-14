"""SH-707 contract regression for an explicitly supplied personal hook source.

Run: PERSONAL_HOOK=/path/to/git-readonly-allow.py python3 test_personal_hook.py
The supplied file is executed with synthetic stdin; payload commands never run.
"""

import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stdout


class PersonalHookContract(unittest.TestCase):
    """Preserve classification while checking the provider output boundary."""

    def _run(self, command="pwd", codex=True, tool="Bash", raw=None):
        source = Path(os.environ["PERSONAL_HOOK"]).resolve()
        with tempfile.TemporaryDirectory(prefix="sh707-personal-", dir="/tmp") as scratch:
            payload = {
                "tool_name": tool,
                "hook_event_name": "PreToolUse",
                "tool_input": {"command": command},
                "cwd": scratch,
            }
            if codex:
                payload["turn_id"] = "test-turn"
            result = subprocess.run(
                [sys.executable, str(source)],
                input=raw if raw is not None else json.dumps(payload),
                text=True, capture_output=True, timeout=5, cwd=scratch,
                env={"PATH": os.environ["PATH"], "HOME": scratch},
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, "")
        return json.loads(result.stdout) if result.stdout.strip() else {}

    def test_codex_readonly_approval_is_neutral(self):
        """Every approved command family defers to Codex permission handling."""
        for command in ("pwd", "git status", "ls -la && cat file.txt"):
            with self.subTest(command=command):
                self.assertEqual(self._run(command), {}, "unsupported permissionDecision:allow")

    def test_claude_readonly_approval_is_preserved(self):
        """Claude retains its existing explicit approval envelope."""
        output = self._run(codex=False)["hookSpecificOutput"]
        self.assertEqual(output["permissionDecision"], "allow")
        self.assertEqual(output["permissionDecisionReason"], "Read-only operation")

    def test_existing_passthroughs_are_preserved(self):
        """Unknown and branch-switch commands keep the existing neutral result."""
        for codex in (False, True):
            for command in ("unknown-command", "git checkout main", "git switch main"):
                with self.subTest(codex=codex, command=command):
                    self.assertEqual(self._run(command, codex), {})

    def test_unrelated_tool_and_malformed_json_are_inert(self):
        """Unmatched tools and unreadable JSON do not produce decisions."""
        self.assertEqual(self._run(tool="Read"), {})
        self.assertEqual(self._run(raw="{"), {})

    def test_denial_serializer_remains_available(self):
        """Preserve the real denial serializer even though current classification defers."""
        spec = importlib.util.spec_from_file_location("personal_hook", os.environ["PERSONAL_HOOK"])
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        output = io.StringIO()
        with redirect_stdout(output):
            module._emit_decision("deny", "Policy denied")
        self.assertEqual(json.loads(output.getvalue())["hookSpecificOutput"], {
            "hookEventName": "PreToolUse", "permissionDecision": "deny",
            "permissionDecisionReason": "Policy denied",
        })


if __name__ == "__main__":
    unittest.main(verbosity=2)
