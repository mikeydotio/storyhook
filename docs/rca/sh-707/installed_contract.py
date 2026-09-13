"""Exercise installed Greenlight manifest commands with inert payload data."""

import json
import os
from pathlib import Path
import subprocess
import tempfile

# The packaged hook declares a 20-second timeout; allow capture overhead too.
HOOK_DEADLINE = 30


def exercise(root, *, repaired):
    """Return exact contract observations, preserving real classifiers and JSON."""
    root = Path(root).resolve()
    manifest = json.loads((root / "hooks/hooks.json").read_text())
    entries = manifest["hooks"]["PreToolUse"]
    if len(entries) != 1 or len(entries[0]["hooks"]) != 1:
        raise ValueError("unexpected installed hook registration")
    hook = entries[0]["hooks"][0]
    if hook["type"] != "command":
        raise ValueError("installed hook is not a command")
    records = []
    with tempfile.TemporaryDirectory(prefix="sh707-wire-", dir="/tmp") as directory:
        scratch = Path(directory)
        config = scratch / ".config/greenlight/config.yaml"
        config.parent.mkdir(parents=True)
        bin_dir = scratch / "bin"
        bin_dir.mkdir()
        curl = bin_dir / "curl"
        curl.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > "$GL_CURL_LOG"\n'
                        'cat "$GL_RESPONSE_FILE"\n')
        curl.chmod(0o755)
        for provider in ("codex", "claude"):
            cases = ["safe", "read", "deny", "warning", "malformed", "unrelated"]
            if repaired:
                cases += ["ai-quiet", "ai-context", "ai-destructive", "ai-invalid"]
            for case in cases:
                command = "rm -rf never-execute-this-fixture" if case in ("deny", "warning") else "pwd"
                if case.startswith("ai-"):
                    command = "unknown-readonly-tool --inspect"
                payload = {"hook_event_name": "PreToolUse", "tool_name": "Bash",
                           "tool_input": {"command": command}, "cwd": directory,
                           "permission_mode": "default"}
                if provider == "codex":
                    payload["turn_id"] = "sh707-fixture"
                if case in ("read", "unrelated"):
                    payload["tool_name"] = "Read" if case == "read" else "SomeFutureTool"
                ai = case.startswith("ai-")
                config.write_text(f"ai_enabled: {str(ai).lower()}\n"
                                  f"ai_show_rationale: {str(case == 'ai-context').lower()}\n"
                                  f"log_file: {scratch}/decisions.log\n")
                rationale = 'Fixture says "safe"; $(never-execute) remains data.'
                answer = True if case == "ai-destructive" else "false" if case == "ai-invalid" else False
                (scratch / "response.json").write_text(json.dumps({"content": [{
                    "type": "text", "text": json.dumps({"answer": answer, "rationale": rationale})}]}))
                log = scratch / "decisions.log"
                if log.exists():
                    log.unlink()
                env = {"PATH": f"{bin_dir}:{os.environ['PATH']}", "HOME": directory,
                       "TMPDIR": "/tmp", "LC_ALL": "C", "GREENLIGHT_PLAN_EXPLORER": str(int(case == "deny")),
                       "ANTHROPIC_API_KEY": "sh707-test-sentinel",
                       "GL_CURL_LOG": str(scratch / "curl.log"),
                       "GL_RESPONSE_FILE": str(scratch / "response.json")}
                # Baseline's Claude-only command needs its historical root variable.
                root_key = "PLUGIN_ROOT" if provider == "codex" and repaired else "CLAUDE_PLUGIN_ROOT"
                env[root_key] = str(root)
                result = subprocess.run(["bash", "-c", hook["command"]],
                                        input="{" if case == "malformed" else json.dumps(payload),
                                        text=True, capture_output=True, cwd=scratch, env=env,
                                        timeout=HOOK_DEADLINE)
                output = json.loads(result.stdout) if result.stdout.strip() else {}
                detail = output.get("hookSpecificOutput", {})
                log_text = log.read_text() if log.exists() else ""
                record = {"provider": provider, "case": case, "root_variable": root_key,
                          "stdout": result.stdout, "stderr": result.stderr, "exit_status": result.returncode,
                          "log": log_text}
                records.append(record)
                if result.returncode or result.stderr:
                    raise ValueError(f"installed hook invocation failed: {record!r}")
                expected = None
                if case == "deny":
                    expected = "deny"
                elif case in ("safe", "read", "ai-quiet", "ai-context"):
                    expected = "allow" if provider == "claude" or not repaired else None
                if detail.get("permissionDecision") != expected:
                    raise ValueError(f"installed decision contract failed: {record!r}")
                if expected is None and any(key in detail for key in ("permissionDecision", "permissionDecisionReason")):
                    raise ValueError(f"neutral output contains decision fields: {record!r}")
                if expected and not detail.get("permissionDecisionReason"):
                    raise ValueError(f"missing decision reason: {record!r}")
                if case in ("ai-quiet", "ai-context") and "AI_RESULT] answer=false" not in log_text:
                    raise ValueError(f"safe AI path was not reached: {record!r}")
                if case == "ai-quiet" and "additionalContext" in detail:
                    raise ValueError(f"disabled rationale appeared on wire: {record!r}")
                if case == "ai-context" and rationale not in detail.get("additionalContext", ""):
                    raise ValueError(f"AI rationale lost: {record!r}")
                if case == "ai-invalid" and "AI_FAIL] invalid structured response" not in log_text:
                    raise ValueError(f"invalid AI type accepted: {record!r}")
                if case in ("warning", "ai-destructive") and not detail.get("additionalContext"):
                    raise ValueError(f"warning lost: {record!r}")
                if case in ("malformed", "unrelated") and output:
                    raise ValueError(f"inert payload produced output: {record!r}")
    return records
